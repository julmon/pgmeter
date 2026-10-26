# Contributing

Issues and pull requests are welcome at
[github.com/julmon/pgmeter](https://github.com/julmon/pgmeter).

## Building

```bash
cargo build            # debug
cargo build --release  # optimized, what you benchmark with
```

Rust 1.87 or later. There are no system dependencies: TLS is rustls, the Lua
runtime (Luau) is compiled in, the chart library is vendored.

## Checks

The CI runs these on every pull request, on stable Rust and on the minimum
supported version:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The end-to-end tests need a PostgreSQL server and are skipped without one.
Point `PGMETER_TEST_DSN` at a database you can afford to wipe, for example a
container:

```bash
docker run -d --name pgtest -p 5432:5432 -e POSTGRES_PASSWORD=pgmeter postgres:18
PGMETER_TEST_DSN=postgresql://postgres:pgmeter@localhost:5432/postgres cargo test
```

They run `init` and short `run`s of the bundled and ad-hoc specs against it,
on PostgreSQL 12 and the current major in CI. Tests that create tables use
their own table names, because they run in parallel.

## Documentation

This site is an [mdBook](https://rust-lang.github.io/mdBook/) under `docs/`
and is deployed by a workflow on every push to `main`:

```bash
cargo install mdbook --version 0.5.4 --locked
mdbook serve docs --open
```

The changelog page includes `CHANGELOG.md` from the repository root.

## Project layout

```
src/
├── main.rs                    # Entry point: dispatches the commands, maps errors to exit codes
├── args.rs                    # CLI argument parsing (clap)
├── spec.rs                    # Embedded benchmark specs (`spec list` / `spec new`)
├── executor.rs                # Executor: connection, spec, aggregation, results, report
└── executor/
    ├── init.rs                # The `init` pipeline (schema, parallel loading, keys, indexes)
    ├── run.rs                 # The `run` pipeline (both client engines, data collector, logs)
    ├── error.rs               # The executor error type and the server-message helper
    ├── tls.rs                 # rustls connector for `--sslmode`
    ├── benchmark.rs           # Transaction and counter types shared by the pipelines
    ├── yaml_types.rs          # Serde structs for the YAML spec
    ├── yaml_benchmark.rs      # Spec validation, schema DDL, loading and transaction execution
    ├── yaml_lua.rs            # Lua VM, DB bridge, binding/template compilation
    ├── data_agg.rs            # Per-transaction response-time statistics
    ├── summary.rs             # Throughput and error-rate math shared by every output
    ├── report.rs              # Self-contained HTML report generation
    ├── terminal.rs            # Terminal output formatting
    ├── txmessage.rs           # Inter-thread messaging
    └── assets/                # Vendored assets (inlined Chart.js)
samples/                       # The bundled benchmark specs, embedded in the binary
docs/                          # This site (mdBook)
tests/                         # End-to-end tests (need `PGMETER_TEST_DSN`)
```

## Conventions

- One topic per pull request, with a commit message that says why.
- A change in behaviour comes with its test and its line in `CHANGELOG.md`
  under "Unreleased".
- The bundled workloads stay derived from the public TPC specifications and
  keep the wording of the trademark notice; see
  [License and trademarks](license.md).
