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
| `once` | `true` for a `--once` run; `duration_s` is then the window the run took |
| `seed` | The `--seed` the clients' draws were seeded from; `null` when they were seeded from the OS |
| `total_time_ms` | Wall-clock time of the whole run, ramp-up included |
| `rampup_time_ms` | Wall-clock time of the ramp-up |
| `workload_secs` | The measured window in seconds (`total_time_ms` minus `rampup_time_ms`, at least 1), the divisor of every throughput figure |
| `rampup_end_ts` | The `transaction.log` timestamp at which the measured window starts |
| `throughput_unit` | The spec's unit, such as `TPM`, `TPS` or `queries/hr` |
| `throughput_factor` | Seconds per unit of throughput: 60 for TPM, 1 for TPS, 3600 per hour |
| `interrupted` | `true` when the run was ended with Ctrl-C |
| `system` | One object per `--sysmon` host, described below; `[]` without the option |

Each entry of `system`:

| Key | Meaning |
|---|---|
| `target` | The `--sysmon` value, without its `db:` prefix |
| `host`, `kernel` | What the host reports as its name and kernel release |
| `cpus`, `mem_total_kb` | Its number of CPUs and its memory |
| `interval_s` | `--sysmon-interval` |
| `samples` | Samples received over the whole run |
| `truncated` | `true` when the sampler ended before the run did |
| `database` | `true` on the host PostgreSQL runs on: the one marked `db:`, else the one the server's address belongs to, else the first |
| `files` | The host's three CSV files, described below |
| `devices` | Its block devices: `name`, `mapper_name` (of a device-mapper volume, else `null`), `on` (the devices directly under it), `top` (no device sits on it) and `holds` (what PostgreSQL keeps on it: `data`, `WAL`, `tablespace <name>`; empty when unknown) |

## `transaction.log`

One line per committed transaction, ramp-up included, in the order the
collector received them, flushed at least every second while the run goes:

```
<timestamp> <active_clients> <transaction_id> <response_time_ms> <client_id> <start_ms>
1793005977 1 3 4.893 1 10412
```

`active_clients` is the number of distinct clients seen so far, which grows
during the ramp-up. `transaction_id` is the `id` from the spec. `client_id`
is the client that ran the transaction, from 1 to `-c`. `start_ms` is when
the transaction started, in milliseconds since the run's collector started,
so `start_ms` plus `response_time_ms` is when it ended. Lines with a
timestamp before `rampup_end_ts` belong to the ramp-up.

## `error.log`

One line per failed transaction:

```
<timestamp> <active_clients> <transaction_id> <message>
```

The message is the server error or the Lua error, with its detail and, for
Lua, its traceback. Line breaks, tabs and backslashes inside it are escaped
as `\n`, `\r`, `\t` and `\\`, so one line is always one error.

A script that fails the same way many times, as a workload with designed
rollbacks does, gets its traceback once: the first error of a transaction
with a given message is logged in full, the next ones with the message
alone. To find where a short line comes from, look for the first line of
that transaction with the same message. pgmeter remembers 4096 distinct
messages per run; past that, new ones are logged in full every time.

## `pgmeter-throughput-<name>.csv` and `pgmeter-throughput-all.csv`

Throughput per second over the whole run, one file per transaction and one
for all of them together:

```
time_s,throughput
0,48480
1,105120
```

`time_s` counts from the first second of the run in the `all` file and from
the first second in which that transaction committed in the per-transaction
files; `throughput` is the second's
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

## `pgmeter-system-<host>-cpu.csv`

Written with `--sysmon`, like the two files below. `<host>` is `local`, or
the name the remote host reports, with `-2` added to the second of two
hosts of the same name. One row per sample, ramp-up included; `timestamp`
is Unix seconds with milliseconds, on the clock of the machine pgmeter runs
on, so its integer part compares with `transaction.log`. A figure the host
does not report is an empty cell, never a zero.

| Column | Meaning |
|---|---|
| `user_pct` ... `idle_pct` | Share of CPU time in each state over the interval, all CPUs together: `user`, `nice`, `system`, `iowait`, `irq`, `softirq`, `steal`, `guest`, `guest_nice`, `idle`. They sum to 100; guest time is not counted again in `user` |
| `ctxt_s`, `forks_s` | Context switches and processes created per second |
| `procs_running`, `procs_blocked` | Tasks runnable and tasks blocked on I/O when the sample was taken |
| `load1`, `load5`, `load15` | Load averages |
| `psi_cpu_some_pct`, `psi_cpu_full_pct`, `psi_memory_*`, `psi_io_*` | Pressure stall information: share of the interval some task, or every task, waited for the resource. Linux 4.20 or later, when enabled |

## `pgmeter-system-<host>-memory.csv`

One row per sample. After `timestamp` come the fields of `/proc/meminfo`
under their kernel names, in kB except the huge page counts: `MemTotal`,
`MemFree`, `MemAvailable`, `Buffers`, `Cached`, `Shmem`, `AnonPages`,
`Mapped`, `Dirty`, `Writeback`, `Slab`, `SReclaimable`, `SUnreclaim`,
`PageTables`, `KernelStack`, `SwapTotal`, `SwapFree`, `SwapCached`,
`CommitLimit`, `Committed_AS`, `HugePages_Total`, `HugePages_Free`,
`HugePages_Rsvd`, `Hugepagesize`. Then the rates per second of these
`/proc/vmstat` counters: `pgpgin_s` and `pgpgout_s` (kB paged in and out),
`pswpin_s` and `pswpout_s` (pages swapped in and out), `pgfault_s`,
`pgmajfault_s` and `oom_kill_s`.

## `pgmeter-system-<host>-disk.csv`

One row per sample and device, with the figures of `iostat -x`. Every whole
device that has done I/O since boot has its rows, every layer of a stack
included; partitions, loop and RAM devices have none.

| Column | Meaning |
|---|---|
| `device` | The kernel name, as in `/proc/diskstats`; `run.json` maps it to a mapper name and a stack |
| `r_s`, `w_s` | Reads and writes completed per second |
| `rkb_s`, `wkb_s` | kB read and written per second |
| `r_await_ms`, `w_await_ms` | Average time of a read and of a write, queue included; 0 without any |
| `rareq_kb`, `wareq_kb` | Average size of a read and of a write |
| `aqu_sz` | Average number of requests waiting or in progress |
| `util_pct` | Share of the interval with at least one request in progress |
| `d_s`, `dkb_s` | Discards per second and kB discarded per second (Linux 4.18) |
| `f_s`, `f_await_ms` | Flushes per second and their average time (Linux 5.5); counted on physical disks only |

## `report.html`

The report, self-contained: the chart library is inlined and the page opens
offline. It reads nothing from the other files; the figures are embedded at
generation time.
