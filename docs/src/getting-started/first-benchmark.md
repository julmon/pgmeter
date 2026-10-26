# Your first benchmark

This page runs the bundled PGM-B workload, a banking debit/credit transaction
derived from TPC-B, against a database of your own, and shows what comes out.
It takes about five minutes.

## 1. A database to benchmark

`init` drops and recreates the tables a spec declares, so use a database you
can afford to wipe:

```bash
export PGPASSWORD=secret
psql -h db -U postgres -c "CREATE DATABASE benchmark"
```

pgmeter reads the same connection settings as `psql`: `-H`/`PGHOST`,
`-p`/`PGPORT`, `-U`/`PGUSER`, `-d`/`PGDATABASE`, `PGPASSWORD` and
`--sslmode`/`PGSSLMODE`. The examples below pass the host, user and database
on the command line; exporting the environment variables once is shorter.

## 2. A copy of the spec

The bundled workloads are embedded in the binary. `spec new` writes an
editable copy of one into a directory, which is what `init` and `run` take:

```bash
pgmeter spec new pgm-b -o .
ls pgm-b
```

The directory holds `spec.yaml`, the whole benchmark: the three tables and
their indexes, how to fill them and the one transaction with its weights and
SQL. Workloads with Lua scripts also get a `scripts/` folder.

## 3. Load the data

```bash
pgmeter -H db -U postgres -d benchmark init --spec pgm-b -s 10 -j 4
```

`-s 10` is the scale factor: PGM-B's unit is a branch, with 10 tellers and
100,000 accounts each, so this creates one million accounts. `-j 4` loads with
four parallel connections. The steps print as they run: schema, loading,
primary keys, foreign keys, indexes, vacuum, checkpoint. At scale 10 this
takes a few seconds.

## 4. Run the workload

```bash
pgmeter -H db -U postgres -d benchmark run --spec pgm-b -c 8 -T 60 -r 10 -P 10
```

Eight clients run for 60 seconds after a 10 second ramp-up during which they
start one by one; `-P 10` prints the throughput and average latency every 10
seconds. The ramp-up is excluded from every statistic. Press Ctrl-C to end a
run early with the results of what ran so far.

## 5. Read the results

The console ends with a headline line (throughput in the spec's unit,
committed transactions, error rate, average response time), a results table
with one row per transaction and a response-time table with the average,
minimum, maximum, standard deviation and the 95th and 99th percentiles.

A results directory named after the start time, like
`pgmeter-20260101T120000Z/`, holds `report.html`, a self-contained page with
the same figures and charts of throughput, latency and errors over time, plus
the raw transaction and error logs and per-second CSV files. Open the report
in a browser; it needs no network.

[Reading the results](../guide/results.md) describes each file and figure.

## Next

- Run the same spec against two configurations and compare the headline
  figures: that is what the tool is for.
- [Bundled workloads](../guide/workloads.md) lists the other five and what
  each models.
- [Writing benchmarks](../writing/index.md) explains the spec you just copied
  and how to write one for your own schema and queries.
