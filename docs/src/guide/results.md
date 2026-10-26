# Reading the results

A run reports in three places: the console, a results directory, and the
HTML report inside that directory. All three describe the steady-state
window, the `-T` seconds after the ramp-up; only the time-series charts and
the raw logs include the ramp-up.

## The console

With `--format table` (the default), the run ends with:

- **The headline**: overall throughput in the spec's unit (TPM for the OLTP
  workloads, queries per hour for the analytical ones), the number of
  committed transactions, the error rate and the average response time. For
  example `24 040 TPM · 24 103 committed · 0.43% errors · 19.82 ms avg`.
- **Results**, one row per transaction: committed count, errors, error rate,
  throughput in the spec's unit.
- **Response times**, one row per transaction: average, minimum, maximum,
  standard deviation, 95th and 99th percentiles, in milliseconds.

During the run, `--progress N` prints the throughput and average latency of
the last N seconds; on a terminal a progress bar shows the elapsed time.

`--format json` prints the same figures as one document (`duration_s`,
`interrupted`, `throughput_unit` and a `transactions` array with `name`,
`committed`, `errors`, `error_rate`, `throughput` and a `latency_ms` object
holding `avg`, `min`, `max`, `p95`, `p99` and `stddev`); `--format csv` prints
one row per transaction with the same columns. Both imply `--quiet`.

## The results directory

Each run creates a directory named after its start time in UTC, such as
`pgmeter-20260101T120000Z/`, in the current directory:

| File | Contents |
|---|---|
| `report.html` | The self-contained report described below |
| `run.json` | The run parameters, the elapsed and ramp-up times, the throughput unit, `rampup_end_ts` and whether the run was interrupted |
| `transaction.log` | One line per committed transaction, ramp-up included |
| `error.log` | One line per failed transaction with its message |
| `pgmeter-throughput-<name>.csv` | Throughput per second of each transaction, and `all`, over the whole run |
| `pgmeter-response-time-<name>.csv` | Response time over time of each transaction |
| `pgmeter-stats-<name>.csv` | The statistical summary of each transaction for the steady-state window |

[Output files](../reference/output-files.md) gives the exact format of each.
The two logs are plain text so that a run can be re-analysed with other tools;
`rampup_end_ts` in `run.json` is the `transaction.log` timestamp at which the
steady-state window starts.

## The HTML report

`report.html` opens offline in any browser: the chart library is inlined and
nothing is fetched. It shows:

- An environment header: benchmark name and description, PostgreSQL version,
  key server settings (`shared_buffers`, `work_mem`, `max_wal_size`,
  `max_connections`, `effective_cache_size`) and the run parameters.
- The headline figures: throughput, peak throughput, committed transactions,
  errors and error rate, average latency.
- The per-transaction results table.
- Charts: throughput over time, latency over time, response-time percentiles,
  the response-time distribution, the committed transaction mix and errors
  over time. The ramp-up window is shaded.

![The charts of a report](../images/report-charts.png)

[View a sample report](../sample-report.html): a PGM-C run with 10 warehouses
and 8 clients for 60 s after a 10 s ramp-up. Hover the charts; the page has a
light and a dark theme.

## Comparing runs

The figures that matter for a comparison are the headline throughput and
the per-transaction percentiles, at the same scale factor, client count and
duration. A few habits make the comparison hold:

- Reload with `init` before each run, so every run starts from the same data.
- Discard the first run after a load, or make it a warm-up: the first
  autovacuum and autoanalyze pass lands during it.
- Use a ramp-up long enough for every client to be running and the caches to
  settle, and a measured window of at least a minute or two.
- Repeat each configuration at least twice and look at the spread before
  reading anything into a difference.
- Watch the server log for checkpoints: a small `max_wal_size` turns a
  write-heavy run into a series of checkpoints that distort it.
