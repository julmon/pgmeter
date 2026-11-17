# Monitoring the system

A throughput figure says what the server did, not why it stopped there. With
`--sysmon`, `pgmeter run` also samples the CPU, the memory and the disks of
the machines you name, for the length of the run, and puts them on the same
time axis as the throughput.

```sh
pgmeter run --spec pgm-c -c 32 -T 600 -r 60 \
    --sysmon postgres@db1 \
    --sysmon local
```

Nothing is monitored unless you ask: without `--sysmon`, pgmeter starts no
sampler and opens no SSH connection.

## Which machines

`--sysmon` takes a target and can be given several times:

| Target | Machine |
|---|---|
| `local` | The one pgmeter runs on |
| `[user@]host[:port]` | A remote host, reached with your system's `ssh` |
| a `Host` alias of `~/.ssh/config`, or an `ssh://` URI | The same |

The usual three to watch are the database server, the machine pgmeter runs
on (a load generator out of CPU gives a wrong result that looks right), and
a standby when replication is synchronous.

The monitored machines must run Linux. They need nothing installed: the
sampler is a short shell script that pgmeter sends over the connection, and
it reads `/proc` and `/sys`. It costs about half a percent of one CPU at one
sample per second.

`--sysmon-interval <seconds>` sets the time between two samples, from 1 to
3600, 1 by default.

## SSH

pgmeter runs your `ssh`, so your keys, your agent, `~/.ssh/config`, jump
hosts and `known_hosts` all apply. It never asks for a password: the
connection must work without a prompt.

```sh
ssh -o BatchMode=yes postgres@db1 true    # must succeed in silence
```

Each host must deliver a first sample within 15 seconds. One that does not
fails the run before any client starts, with what `ssh` said, so that a
ten-minute run is not spent without its metrics. `PGMETER_SSH` replaces the
`ssh` program, with arguments if needed:
`PGMETER_SSH="ssh -F ./ssh_config"`.

A sampler that dies during the run does not stop it. Its series ends there,
a `warning:` line says so when the run ends, and the report marks the host
as truncated.

## The database server among the hosts

The report opens on the host PostgreSQL runs on, and labels its volumes.
pgmeter finds it by asking PostgreSQL which address it accepted the
connection on and matching it against the addresses of the monitored hosts;
a Unix socket or a loopback address means `local`.

Behind a connection pooler, a load balancer or NAT, that address belongs to
none of them, and pgmeter falls back to the first target. Say which one it
is with the `db:` prefix:

```sh
pgmeter run --spec pgm-c -H pooler1 --sysmon db:postgres@db1 --sysmon local
```

Only one target can carry it. A host named `db` with a port, `db:2222`, is
still read as a host and a port.

## Data and WAL volumes

When the role pgmeter connects as may read the `data_directory` setting (a
superuser, or a member of `pg_read_all_settings`), pgmeter looks up, on the
database server, which volume holds the data directory, which holds
`pg_wal`, and which holds each tablespace. The report then says `data`,
`WAL` or `data + WAL` next to the volume names, opens the disk charts on the
data volume, and shows the flush time of the WAL disk as a headline figure:
that is how long a commit waits for the disk.

```sql
GRANT pg_read_all_settings TO bench;
```

Without the privilege, or when the SSH user cannot enter the data directory
to follow the `pg_wal` link, the volumes are simply not labelled. A file
system without a single block device under it (ZFS, Btrfs on several disks,
network storage) cannot be labelled either.

## What you get

On the console, after the transaction tables, one row per host and one per
volume, over the measured window:

```text
System:
        Host           CPU busy (%)   Max. (%)   User (%)   System (%)   I/O wait (%)   Mem. used (GiB)   Max. (GiB)   I/O pressure (%)
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 db1 (database)                54.4       60.1       36.1         14.0           11.9               9.2          9.3               22.0

Volumes:
   Host           Volume           Holds      Written (MB/s)   Read (MB/s)   Requests/s   Write (ms)   Flush (ms)   Busy (%)   Max. (%)
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 db1           vg_data-pgdata      data                 25.3           0.0        1 577         0.82         1.28       90.6       96.6
```

`--format json` carries the same figures in a `system` array. In the results
directory, each host has three CSV files with every sample, ramp-up
included; [Output files](../reference/output-files.md) lists their columns.

The HTML report gains a System section, with a host selector when several
hosts were monitored:

- Six headline figures for the measured window: CPU busy, I/O wait, memory
  used, MB/s written to all volumes, the busiest volume, and the WAL flush
  time (or the I/O pressure when the WAL volume is not known).
- **CPU**: time by state, run queue and load against the number of CPUs,
  pressure, context switches, processes created.
- **Memory**: use by class, dirty and writeback, paging, page faults, swap
  and major faults.
- **Disk**: a table of the volumes, then for the device you pick its
  throughput, requests, request time, request size and queue size, and the
  share of time each volume was busy.

Every chart shares the time axis of the throughput chart, with the ramp-up
shaded, so a drop in throughput can be read straight down the page. The
[sample throughput report](../sample-report.html) ends with one.

## Reading the disk figures

**Volumes, not disks.** A logical volume on an encrypted partition on a disk
is three devices to the kernel, each counting the same I/O. The report shows
the top one, the device the file system sits on, because it is what
PostgreSQL waits for; the layers under it are in the device selector. On a
multipath volume or a RAID array this is the only view that means anything.
A volume that never reached 1% busy during the run, such as a swap volume
touched a few times, is left out and named under the charts.

**Flushes belong to the disk.** A flush is what `fsync` costs: PostgreSQL
waits for one at every commit. The kernel counts flushes on the physical
disk, not on the volumes above it, so a volume's charts show the flushes of
the disk under it, and say which. Two volumes on the same disk share that
disk's flushes. Storage with a non-volatile cache (a controller with a
battery, some cloud disks) is never sent a flush: the line stays at zero
there although PostgreSQL still syncs.

**Busy is not saturated.** "Time busy" is the share of time the device had
at least one request in progress. A single spinning disk at 100% is full; a
RAID array, a multipath volume or an NVMe disk serves many requests at once
and can do more at 100%. On those, the request time is the figure to watch:
it rises when the device is really behind.

**Request time includes the queue.** It is the time from the request
entering the kernel's queue to its completion, as `iostat -x` reports it.

## What is not collected

Network traffic, per-CPU and per-process figures, NUMA and cgroup
accounting are not collected, and `init` cannot be monitored: `--sysmon`
belongs to `run`. Hosts that are not Linux are refused when the run starts.

The samples are placed on the clock of the machine pgmeter runs on, the same
as `transaction.log`, so the clocks of the monitored hosts need not agree
with it. A sample is what happened between two reads of the counters, up to
the moment pgmeter received it; over a slow link that moment is late by the
transfer time.
