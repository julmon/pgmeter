# Installation

## Requirements

- Rust 1.87 or later to build (declared as `rust-version` in `Cargo.toml`).
- PostgreSQL 12 or later to benchmark. pgmeter connects over the network like
  `psql` does; it does not need to run on the database server.
- Linux or macOS. pgmeter is developed and tested on Linux; Windows is not
  supported.

## From source

```bash
git clone https://github.com/julmon/pgmeter.git
cd pgmeter
cargo build --release
```

The binary is `target/release/pgmeter`. Copy it anywhere on your `PATH`; it
has no runtime dependencies. The bundled workloads, the Lua runtime and the
report's chart library are embedded in it, so a source checkout is not needed
after the build.

TLS uses rustls with the system certificate store, so nothing has to be
installed for `--sslmode`; see [Connecting to PostgreSQL](../guide/connecting.md).

## Checking the installation

```bash
pgmeter --version
pgmeter spec list
```

`spec list` prints the bundled workloads with their description. Next:
[your first benchmark](first-benchmark.md).
