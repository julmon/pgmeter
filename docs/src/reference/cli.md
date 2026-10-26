# Command line

```
pgmeter [connection options] <command> [options]
```

Commands: `init`, `run`, `spec list`, `spec new`. `pgmeter --help` and
`pgmeter <command> --help` print the same options with their defaults.

## Connection options

Global, placed before the command. Each has the environment variable the
PostgreSQL tools use; see [Connecting to PostgreSQL](../guide/connecting.md).

| Option | Environment variable | Default | Description |
|---|---|---|---|
| `-H, --host <HOSTNAME>` | `PGHOST` | localhost | Host name or socket directory |
| `-p, --port <PORT>` | `PGPORT` | 5432 | Port |
| `-U, --username <USERNAME>` | `PGUSER` | current OS user | Database user |
| `-d, --dbname <DBNAME>` | `PGDATABASE` | current OS user | Database |
| `--sslmode <MODE>` | `PGSSLMODE` | prefer | `disable`, `prefer`, `require`, `verify-ca` or `verify-full` |
| `-h, --help` | | | Print help |
| `-V, --version` | | | Print the version |

The password comes from `PGPASSWORD`, or from the password file
(`PGPASSFILE`, default `~/.pgpass`) when it is not set.

## `init`

```
pgmeter [connection options] init --spec <DIR|FILE> [options]
```

Drops and recreates the tables the spec declares, loads the data, then adds
keys and indexes, vacuums and checkpoints. See
[Initializing a benchmark](../guide/initializing.md).

| Option | Default | Description |
|---|---|---|
| `--spec <PATH>` | required | A benchmark directory containing `spec.yaml`, or a single spec file |
| `-s, --scalefactor <NUM>` | 1 | Number of scale units to load |
| `-j, --jobs <NUM>` | 1 | Parallel loader connections |
| `--no-fkey` | off | Skip foreign-key creation |
| `--post-create-table <FILE>` | none | SQL file run after the tables are created |
| `-q, --quiet` | off | No progress output |

## `run`

```
pgmeter [connection options] run --spec <DIR|FILE> [options]
```

Drives the spec's transaction mix. See [Running a benchmark](../guide/running.md).

| Option | Default | Description |
|---|---|---|
| `--spec <PATH>` | required | The spec `init` was given |
| `-c, --client <NUM>` | 1 | Concurrent clients |
| `-T, --time <NUM>` | 1 | Measured duration in seconds |
| `-r, --rampup <NUM>` | 0 | Ramp-up in seconds, excluded from the statistics |
| `-P, --progress <NUM>` | 0 | Print live progress every NUM seconds; 0 disables |
| `-j, --threads <NUM>` | 0 | Worker threads of the async engine (simple-mode specs); 0 is half the CPUs, at most one per client |
| `--format <FMT>` | table | `table`, `json` or `csv` |
| `-q, --quiet` | off | No progress output; only the result data on stdout |

## `spec list`

```
pgmeter spec list
```

Prints the bundled workloads with their title and description.

## `spec new`

```
pgmeter spec new <BENCHMARK> [-o <DIR>] [--force]
```

Writes an editable copy of a bundled workload: its `spec.yaml` and, when it
has any, its `scripts/` folder.

| Argument or option | Default | Description |
|---|---|---|
| `<BENCHMARK>` | required | A name from `spec list`: `pgm-b`, `pgm-c`, `pgm-c-client`, `pgm-h`, `pgm-ds`, `pgm-iot` or `custom-simple` |
| `-o, --output <DIR>` | `.` | Directory the benchmark folder is written into |
| `--force` | off | Overwrite the destination if it exists |

## Exit status

| Status | Meaning |
|---|---|
| 0 | Success, including a run ended early with one Ctrl-C |
| 1 | Any error: bad arguments, unreadable or invalid spec, connection failure, a client that could not start, an `init` step that failed |
| 130 | A second Ctrl-C during a run, which aborts immediately |

Errors are printed on stderr as `error: <message>`, with the server's detail
when there is one.
