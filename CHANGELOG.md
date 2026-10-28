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
  a results summary as a table, JSON or CSV (`--format`, `--quiet`). Specs
  made of `simple` transactions run on an async engine: a pool of worker
  threads (`--threads`, default half the CPUs) drives one task per client,
  which wakes the process up far less often than one thread per client did
  (PGM-B went from 87% to 94% of pgbench's rate on the same box); specs with
  a `script` transaction run one thread per client. `run --once` replaces
  the timed random stream with the TPC-H/DS stream model: each client runs
  every transaction once in its own random order, then stops. PGM-H's
  queries have equal weights, as the specification's streams give them.
- Benchmarks are YAML specifications plus Lua (Luau) scripts. Transactions run
  in `simple` mode (SQL templates with Lua bindings, sent as one batch or, with
  the list form of `sql`, one round trip per statement like pgbench) or
  `script` mode (Lua with a database bridge: `exec`, `query`, `query_one`,
  `begin`, `commit`, `rollback`, `copy_*`, `rand_*`). Data loading uses either row templates or
  Lua load scripts, with shared modules and a pre-load script.
- Bundled reference workloads derived from the public TPC specifications: PGM-B,
  PGM-C, PGM-H, PGM-DS and PGM-IoT, plus a minimal `custom-simple` template.
  PGM-DS implements all 99 TPC-DS queries with per-execution random parameters.
  PGM-C runs its transactions as PL/pgSQL functions, one round trip each, like
  HammerDB's TPROC-C; PGM-C-client drives the same workload from the client
  statement by statement. Both pick customers, items and last names with TPC-C's
  NURand distribution. `pgmeter spec list` shows them and `pgmeter spec new`
  scaffolds a copy to edit.
- A documentation site at <https://julmon.github.io/pgmeter/>: getting
  started, the user guide, the benchmark authoring guide and the command-line,
  Lua API and output-file reference, built with mdBook from `docs/`. The
  README is a summary that points to it.
- Each run writes a result directory (`pgmeter-<UTC timestamp>/`) with
  `run.json` (run parameters, elapsed and ramp-up times, throughput unit, the
  ramp-up boundary), `transaction.log` (one line per commit with the client
  that ran it and its start time) and `error.log` (one record per line,
  line breaks in messages escaped), per-transaction CSV files
  (`pgmeter-throughput-*.csv` in the spec's unit, response times over time,
  statistics) and a self-contained HTML report with charts, percentiles,
  server and data-set information. Committed counts, throughput, statistics
  and the report's histogram describe the steady-state window after ramp-up;
  the time-series charts show the whole run with ramp-up shaded.
- Specs are validated when loaded: unknown keys, unknown modes or load
  methods, missing `sql` or `script`, duplicate or colliding transaction ids
  and names, a zero total weight, a zero `batch_size`, undeclared tables and
  `load` together with `load_script` are reported together before anything
  touches the database.
- A failed transaction rolls back and the connection stays usable; a script
  that fails mid-COPY has its stream aborted; Lua bridge and loader errors
  carry the PostgreSQL server message. Lua `NaN` and infinite numbers reach
  the server as `float8` literals.
- Ctrl-C ends a run early: the clients stop, the results and the report are
  produced from the steady-state samples so far and marked as interrupted
  (`interrupted` in `run.json`); a second Ctrl-C aborts immediately with
  status 130.
- Connection settings follow libpq conventions: `PGHOST`, `PGPORT`, `PGUSER`,
  `PGDATABASE`, `PGPASSWORD` and `~/.pgpass`. The default user name falls back
  to `$USER`, `$LOGNAME` and `postgres` when the UID has no passwd entry.
- TLS with `--sslmode` / `PGSSLMODE` (`disable`, `prefer`, `require`,
  `verify-ca`, `verify-full`, libpq semantics) through rustls; the `verify-*`
  modes check the server certificate against the system CA store.
- Released under the PostgreSQL License; third-party notices for Chart.js and
  Luau in `THIRD_PARTY_NOTICES.md`.
- Minimum supported Rust version 1.87; PostgreSQL 12 or later; Linux and
  macOS.
- Continuous integration on stable and the MSRV, with end-to-end tests
  against PostgreSQL 12 and 18.

[Unreleased]: https://github.com/julmon/pgmeter/compare/main...HEAD
