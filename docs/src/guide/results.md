# Reading the results

A run reports in three places: the console, a results directory, and the
HTML report inside that directory. All three describe the steady-state
window, the `-T` seconds after the ramp-up; only the time-series charts and
the raw logs include the ramp-up.

## The console

With `--format table` (the default), the run ends with:

- **The headline**: overall throughput in the spec's unit (TPM for PGM-C,
  TPS for PGM-B and PGM-IoT, queries per hour for PGM-H and PGM-DS), the number of
  committed transactions, the error rate and the average response time. For
  example `24 040 TPM · 24 103 committed · 0.43% errors · 19.82 ms avg`.
- **Results**, one row per transaction: committed count, errors, error rate,
  throughput in the spec's unit.
- **Response times**, one row per transaction: average, minimum, maximum,
  standard deviation, 95th and 99th percentiles, in milliseconds.

During the run, a progress bar on the terminal shows the elapsed time, the
current throughput and the average latency; when stderr is not a terminal,
`--progress N` prints the same figures as a `progress:` line every N
seconds. The rate is the one of the last second, not an average since the
start.

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
- Charts: throughput over time, latency over time, the response-time
  distribution and errors over time; for specs with several transactions,
  also the response-time percentiles per transaction and the committed
  transaction mix. The ramp-up window is shaded.

![The charts of a report](../images/report-charts.png)

[View a sample report](../sample-report.html): a PGM-C run with 10 warehouses
and 8 clients for 60 s after a 10 s ramp-up. Hover the charts; the page has a
light and a dark theme.

## Comparing runs

The figures that matter for a comparison are the headline throughput and
the per-transaction percentiles, at the same scale factor, client count and
duration. [Measuring well](measuring.md) lists the habits that make such a
comparison hold: reload before each run, discard the first run after a load,
ramp up and measure long enough, repeat, and watch the server log.
