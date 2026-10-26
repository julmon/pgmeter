# Output files

Every `run` writes a directory named after its start time in UTC,
`pgmeter-<YYYYMMDD>T<HHMMSS>Z/`, in the current directory. Its files are
plain text so that a run can be re-analysed with other tools. Timestamps are
Unix seconds; durations are milliseconds; `<name>` is the transaction's
`name` from the spec.

## `run.json`

The run parameters and timing, one JSON object:

| Key | Meaning |
|---|---|
| `clients` | `-c` |
| `duration_s` | `-T` |
| `rampup_s` | `-r` |
| `total_time_ms` | Wall-clock time of the whole run, ramp-up included |
| `rampup_time_ms` | Wall-clock time of the ramp-up |
| `workload_secs` | The measured window in seconds (`total_time_ms` minus `rampup_time_ms`), the divisor of every throughput figure |
| `rampup_end_ts` | The `transaction.log` timestamp at which the measured window starts |
| `throughput_unit` | The spec's unit, such as `TPM`, `TPS` or `queries/hr` |
| `throughput_factor` | Seconds per unit of throughput: 60 for TPM, 1 for TPS, 3600 per hour |
| `interrupted` | `true` when the run was ended with Ctrl-C |

## `transaction.log`

One line per committed transaction, ramp-up included, in the order the
collector received them:

```
<timestamp> <active_clients> <transaction_id> <response_time_ms>
1793005977 1 3 4.893
```

`active_clients` is the number of distinct clients seen so far, which grows
during the ramp-up. `transaction_id` is the `id` from the spec. Lines with a
timestamp before `rampup_end_ts` belong to the ramp-up.

## `error.log`

One line per failed transaction:

```
<timestamp> <active_clients> <transaction_id> <message>
```

The message is the server error or the Lua error, with its detail and, for
Lua, its traceback. Line breaks, tabs and backslashes inside it are escaped
as `\n`, `\r`, `\t` and `\\`, so one line is always one error.

## `pgmeter-throughput-<name>.csv` and `pgmeter-throughput-all.csv`

Throughput per second over the whole run, one file per transaction and one
for all of them together:

```
time_s,throughput
0,48480
1,105120
```

`time_s` counts from the first ramp-up second; `throughput` is the second's
committed count expressed in the spec's unit (the count times
`throughput_factor`), so a TPM series reads as the rate the second would
sustain over a minute.

## `pgmeter-response-time-<name>.csv`

The average response time of the transactions committed in each second of
the whole run:

```
time_s,response_time_ms
0,1.8445
1,1.8127
```

Seconds in which the transaction did not run are absent.

## `pgmeter-stats-<name>.csv`

The statistical summary of the steady-state window, the same figures as the
results tables:

```
describe,response_time_ms
count,19339
mean,7.1517
std,3.1767
min,2.094
95%,12.7685
99%,17.1971
max,46.398
```

`count` is the number of committed transactions in the window; the other
rows are in milliseconds.

## `report.html`

The report, self-contained: the chart library is inlined and the page opens
offline. It reads nothing from the other files; the figures are embedded at
generation time.
