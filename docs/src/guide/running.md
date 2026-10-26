# Running a benchmark

`run` drives the spec's transaction mix against the data `init` loaded:

```bash
pgmeter [connection options] run --spec <DIR|FILE> [-c N] [-T N] [-r N] [-P N] [-j N] [--format FMT] [-q]
```

| Option | Default | Description |
|---|---|---|
| `--spec` | required | The same spec `init` was given |
| `-c, --client` | 1 | Concurrent clients, one connection each |
| `-T, --time` | 1 | Measured duration in seconds, after the ramp-up |
| `-r, --rampup` | 0 | Ramp-up in seconds: the clients start spread over this window, and nothing in it counts |
| `-P, --progress` | 0 | Every N seconds, print a `progress:` line with the current throughput and average latency (over the last second) when stderr is not a terminal; on a terminal the progress bar shows them live |
| `-j, --threads` | 0 | Worker threads of the async engine (see below); 0 is half the CPUs, at most one per client |
| `--format` | table | `table`, `json` or `csv` for the results on stdout |
| `-q, --quiet` | off | No progress output; `--format json` and `csv` imply it |

Use the same `--spec` for `init` and `run`: the spec is the single source of
truth for the schema and the workload, and `run` reads its scaling query and
transactions from it.

## What a run does

1. Reads the largest scale-unit id from the data (the spec's `max_id_query`),
   so the workload spreads over everything `init` loaded.
2. Starts the clients one by one over the ramp-up window. Each client runs
   transactions back to back, with no think time, picking the next one at
   random according to the spec's weights.
3. At the end of the ramp-up, starts counting. Committed and failed
   transactions and their response times are collected for `-T` seconds.
4. Stops the clients, writes the results directory and prints the summary.

Every transaction is logged with its response time, ramp-up included, but the
headline figures, the per-transaction statistics and the report's histogram
describe the steady-state window only. The time-series charts show the whole
run with the ramp-up shaded.

## The two engines

How the clients are driven depends on the spec, and the run header says which
engine is in use.

- **Simple-mode specs** (every transaction is SQL with bindings, like PGM-B)
  run on the async engine: a pool of `--threads` worker threads drives one
  task per client, like pgbench's `-j`. A worker that finds its client
  waiting for the server moves on to the next client, so the process wakes
  up far less often than one thread per client would, and the load generator
  takes less CPU away from a server on the same machine. The default of half
  the CPUs suits a machine shared with the server; a dedicated client machine
  can use more.
- **Specs with a script transaction** (Lua, like PGM-C and PGM-H) run one
  thread per client on a blocking connection, because the Lua database bridge
  is synchronous. `--threads` is ignored.

## Stopping early

Ctrl-C ends the run at the end of each client's current transaction. The
results and the report cover what ran so far and are marked as interrupted
(`"interrupted": true` in `run.json`), and pgmeter exits normally. A second
Ctrl-C aborts immediately with exit status 130.

## Errors during the run

A transaction that fails, whether the server rejected a statement or a Lua
script raised an error, is counted in the `errors` column and its message is
written to `error.log`. The client rolls back and carries on with the next
transaction; a failed transaction's duration is not counted in the latency
statistics. Some workloads fail on purpose: PGM-C's New-Order rolls back
about 1% of its orders, as TPC-C prescribes, and those show up as errors with
the message `New-Order rollback: unknown item`.

A client that cannot connect at all stops the run with an error and exit
status 1, after the collector has written the logs of what ran.

## Output formats

`--format table` prints the headline and the two tables described in
[Reading the results](results.md). `--format json` prints one JSON document
with the same figures and `--format csv` one CSV table, both for scripting;
they imply `--quiet` so stdout holds only the result data.
