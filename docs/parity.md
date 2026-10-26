# pgmeter against pgbench and HammerDB

The bundled PGM-B, PGM-C and PGM-H workloads are meant to put the same load on
a PostgreSQL server as the tools people already use for TPC-B-, TPC-C- and
TPC-H-like workloads: `pgbench` and HammerDB. This page records how that was
checked, what the numbers were, and what the check changed in pgmeter.

Everything here was measured on one machine, with both tools interleaved on
the same server, so the numbers only mean something relative to each other.
None of this is a TPC result (see the trademark notice in the README).

## Summary

| Workload | Reference tool | Reference result | pgmeter result | pgmeter / reference |
|---|---|---|---|---|
| TPC-B-like, 8 clients | pgbench 18 (simple protocol) | 2,886 and 2,993 TPS | PGM-B: 2,530 and 2,613 TPS | 0.85 to 0.90 |
| TPC-C-like, 10 warehouses, 8 clients | HammerDB 6.1 TPROC-C | 81,706 and 91,503 NOPM | PGM-C: 98,379 and 108,024 New-Orders/min | 1.18 to 1.20 |
| TPC-C-like, same, HammerDB's own schema and functions driven by pgmeter | HammerDB 6.1 TPROC-C | same | 98,163 and 99,693 NOPM | 1.09 to 1.22 |
| TPC-H-like, scale factor 1, 1 client, HammerDB's own schema and queries driven by pgmeter | HammerDB 6.1 TPROC-H | 22 queries in 27.7 and 29.9 s, geometric mean 0.711 and 0.738 s | 22 queries in 29.3 s, geometric mean 0.702 s | 0.95 to 1.06 |
| TPC-H-like, scale factor 1, 1 client, PGM-H's own schema | HammerDB 6.1 TPROC-H | same | PGM-H: 22 queries in 17.8 s, geometric mean 0.482 s | 0.60 to 0.64 (schema differences, see below) |

Each row is two interleaved measurements (reference, pgmeter, reference,
pgmeter), reported as the pair.

## Setup

- Machine: 4 CPUs, 15 GB RAM, Debian 12, kernel 6.1, shared between the
  server, the load generator and unrelated services. Client and server on the
  same box, so a load generator that burns CPU takes it away from the server.
- Server: the `postgres:18` Docker image (PostgreSQL 18.4) with the data
  directory on a tmpfs, so that the comparison is CPU-bound and not disk-bound:

  ```
  docker run -d --name pgparity -p 127.0.0.1:55419:5432 -e POSTGRES_PASSWORD=pg \
    --shm-size=1g --tmpfs /var/lib/postgresql:size=8g postgres:18 \
    -c shared_buffers=1GB -c max_connections=200 -c max_wal_size=2GB \
    -c checkpoint_timeout=30min -c full_page_writes=off
  ```

  `full_page_writes=off` is safe on a volatile data directory and keeps WAL
  volume proportional to the work done; see "Pitfalls" for what the default
  settings did to the numbers.
- pgbench 18.6, HammerDB 6.1 (`tpcorg/hammerdb:latest`, run with
  `--network host`), pgmeter built with `cargo build --release` from the
  commit that merged the changes described below.
- Every measured run was preceded by a `CHECKPOINT`, and the first run after a
  fresh load was never the only measurement (see "Pitfalls").

## TPC-B-like: PGM-B against pgbench

Both tools: scale 10, 8 clients, 60 s, no think time; pgbench in its default
simple-query mode (`pgbench -c 8 -j 4 -T 60`), PGM-B with
`pgmeter run --spec samples/pgm-b -c 8 -T 60 -r 0`.

| Run | pgbench | PGM-B |
|---|---|---|
| A | 2,993 TPS, 2.67 ms | 2,530 TPS, 3.11 ms |
| B | 2,886 TPS, 2.77 ms | 2,613 TPS, 3.01 ms |

Client CPU over the 60 s, from `time`: pgbench 11 s user + 24 s system,
pgmeter 20 s user + 23 s system. The two send the same seven statements per
transaction over the same protocol, the server does the same work, and the
remaining 10 to 15% is the load generator's own CPU on a box where it shares
the four cores with the server. A `perf` profile of the pgmeter client shows
where that CPU goes: 57% in the kernel (loopback TCP, plus about 5% of
connection tracking and firewall rules on this host), 29% in pgmeter and 8%
in libc, with no single hot spot. The largest user-space items are the
statistics thread (2.7%), the string substitution of bindings into the
statements (about 3%), allocation (about 4%) and the async runtime the
synchronous client is built on (about 5%). Closing the gap would mean a
leaner path for simple-mode statements, not a one-line fix; on a machine
where the client does not share CPUs with the server it does not show.

PGM-B as first shipped sent the whole transaction as one multi-statement
string, which is one round trip instead of seven and reported twice pgbench's
TPS. The spec now lists the statements one by one (`sql` as a list), which is
what pgbench does; the batch form is still available for people who want it.

## TPC-C-like: PGM-C against HammerDB TPROC-C

Both tools: 10 warehouses, 8 virtual users or clients, no key-and-think time,
1 min ramp-up, 2 min measured. HammerDB with its PostgreSQL defaults
(`pg_storedprocs false`, so each transaction is one call to a PL/pgSQL
function), PGM-C with `pgmeter run --spec samples/pgm-c -c 8 -T 120 -r 60`.
HammerDB reports NOPM from the district counters and TPM from
`pg_stat_database`; pgmeter reports committed New-Orders and committed
transactions per minute, so the 1% of New-Orders that roll back by design are
counted by HammerDB's TPM and not by pgmeter's.

The middle column is pgmeter driving HammerDB's own PL/pgSQL functions on
HammerDB's own schema, with HammerDB's mix and input generation, from a
throwaway spec. It separates the load generator from the workload definition.

| Run | HammerDB TPROC-C | pgmeter on HammerDB's functions | PGM-C |
|---|---|---|---|
| A (first run after load) | 81,706 NOPM / 189,881 TPM | 99,693 NO/min / 223,303 TPM | 98,379 NO/min / 219,106 TPM |
| B | 91,503 NOPM / 213,499 TPM | 98,163 NO/min / 220,033 TPM | 108,024 NO/min / 241,165 TPM |

Two things follow. pgmeter as a load generator costs the server less than
HammerDB's Tcl virtual users: on identical server-side work it reports 8 to
22% more. And PGM-C's own SQL and schema put a load equivalent to HammerDB's
on the server, at the same WAL volume per run (1.6 GB).

PGM-C started this comparison at 14% of HammerDB's NOPM. What changed:

- The transactions run as PL/pgSQL functions, one round trip each, like
  HammerDB's. The original client-driven version (about 45 round trips per
  New-Order) is kept as `samples/pgm-c-client` for people modelling an
  application without stored procedures.
- The foreign keys and an extra index on `stock (s_w_id, s_i_id, s_quantity)`
  are gone; HammerDB's schema has neither, and they made Stock-Level 5x and
  New-Order 2x slower.
- Customers, items and last names follow TPC-C's NURand distribution.
- Delivery visits the ten districts in order and New-Order updates stock rows
  sorted by key, so concurrent transactions take their locks in the same order
  (the first server-side version deadlocked 0.15% of Deliveries).
- Stock-Level reads `d_next_o_id` first and then queries the 20-order window
  with the value. With the district table joined in, the planner could not
  estimate the window and, after the first autoanalyze of a fresh database,
  hash-joined every order line of the district with every low-stock item of
  the warehouse: 150 ms per call for about a minute, and a third of the run's
  throughput.
- The stock counters New-Order maintains sit next to `s_quantity` instead of
  behind the ten 24-byte district strings. PostgreSQL logs the changed span of
  an updated row; the old layout made PGM-C write 45% more WAL per run than
  HammerDB for the same work.

## TPC-H-like: PGM-H against HammerDB TPROC-H

Both tools: scale factor 1 (6 million order lines), one virtual user or
client. HammerDB runs its 22 queries once per set in a fixed order and reports
the set time and the geometric mean of the query times
(`pg_total_querysets 2`, `pg_verbose false`). PGM-H picks queries at random
with weights and random parameters for as long as it runs
(`pgmeter run --spec samples/pgm-h -c 1 -T 480`), so its figure per query is
the average over the run.

Two comparisons were made. The first is the one that matters for the load
generator: pgmeter running the exact query texts HammerDB sent (taken from
one verbose HammerDB run, so with the same parameters) against HammerDB's own
database. The second is PGM-H's own schema, data and queries.

| TPC-H query | HammerDB, set 1 / set 2 | pgmeter, same queries, same database | PGM-H, own schema and data |
|---|---|---|---|
| Q1 | 3.99 / 4.73 s | 3.98 s | 3.50 s |
| Q2 | 0.38 / 0.40 | 0.30 | 0.27 |
| Q3 | 0.67 / 0.81 | 0.79 | 0.65 |
| Q4 | 0.40 / 0.37 | 0.33 | 0.23 |
| Q5 | 0.52 / 0.48 | 0.41 | 0.33 |
| Q6 | 0.58 / 0.65 | 0.70 | 0.70 |
| Q7 | 0.63 / 0.60 | 0.67 | 1.00 |
| Q8 | 0.27 / 0.25 | 0.29 | 0.26 |
| Q9 | 1.46 / 1.64 | 1.64 | 1.29 |
| Q10 | 1.18 / 1.16 | 1.13 | 0.40 |
| Q11 | 0.14 / 0.15 | 0.11 | 0.11 |
| Q12 | 1.16 / 1.19 | 1.16 | 1.02 |
| Q13 | 1.10 / 1.21 | 1.11 | 0.83 |
| Q14 | 0.60 / 0.60 | 0.61 | 0.22 |
| Q15 | 4.72 / 5.48 | 5.79 | 0.38 |
| Q16 | 0.58 / 0.53 | 0.58 | 0.45 |
| Q17 | 1.92 / 1.80 | 1.76 | 1.10 |
| Q18 | 5.57 / 5.95 | 6.11 | 3.79 |
| Q19 | 0.10 / 0.09 | 0.09 | 0.08 |
| Q20 | 0.29 / 0.26 | 0.25 | 0.19 |
| Q21 | 1.30 / 1.38 | 1.40 | 0.82 |
| Q22 | 0.13 / 0.20 | 0.14 | 0.14 |
| Sum of the 22 | 27.7 / 29.9 s | 29.3 s | 17.8 s |
| Geometric mean | 0.711 / 0.738 s | 0.702 s | 0.482 s |

On HammerDB's schema and queries, pgmeter reports the same query times as
HammerDB: the sum of the 22 is within HammerDB's own set-to-set spread and the
per-query ratios range from 0.72 to 1.14 with no bias. There is no load
generator effect to speak of in a single-stream analytical run; the client
sends one statement and waits.

PGM-H's own set is about 40% faster than TPROC-H's on the same machine, and
the reasons are in the schemas, not in pgmeter:

- HammerDB's PostgreSQL TPROC-H schema declares every key column as
  `numeric` and every date as `timestamp`; PGM-H uses `integer` and `date`,
  as TPC-H's data types suggest. Numeric keys are wider (177 against 154 bytes
  per `lineitem` row) and slower to hash, join and compare, which shows on
  every join-heavy query.
- HammerDB's Q15 creates a view and scans `lineitem` twice through it
  (4.5 to 5.5 s); PGM-H's Q15 uses a CTE and scans once (0.4 s).
- PGM-H has four indexes HammerDB's schema does not have (`lineitem` on
  `l_shipdate`, `l_partkey` and `l_suppkey`, `orders` on `o_orderdate`). With
  those dropped the set takes 18.5 s instead of 17.8 s: Q14 and Q15 move
  toward HammerDB's times and Q7 gets faster (1.0 to 0.47 s, the planner
  preferred the index to a hash join). They are kept, because the comparison
  that counts is the first one and because they are what a PostgreSQL TPC-H
  setup usually has.

Readers who want TPROC-H's numbers from pgmeter can run HammerDB's schema and
queries through pgmeter, as above; readers who want PGM-H's should expect it
to be faster per query set than TPROC-H by roughly this margin on PostgreSQL.

## Pitfalls

Things that produced wrong or noisy numbers before they were understood. They
apply to any tool, not just pgmeter.

- **Checkpoints every few seconds.** The first server used
  `max_wal_size=1GB` and `checkpoint_timeout=5min` with full-page writes on. At
  3,000 transactions per second it checkpointed every 5 to 12 seconds
  ("checkpoints are occurring too frequently" in the log) and wrote 148 GB of
  WAL in a few hours. Both tools lost throughput, unevenly, and run-to-run
  spread reached 30%. Settings passed on the container's command line cannot
  be changed with `ALTER SYSTEM`, and restarting a tmpfs-backed container
  drops its data, so the server was recreated.
- **The first run after a fresh load.** Both tools report about 10% less on
  the first run after loading than on the next ones, as the first autovacuum
  and autoanalyze wave lands during the run. Either discard that run or do a
  throwaway run after loading.
- **Memory on a shared box.** A 10 GB tmpfs plus 2.5 GB of shared buffers plus
  WAL on the same tmpfs pushed the host into swap, and one chain of
  measurements was killed for it. Budget the tmpfs, the buffer cache and
  `max_wal_size` together against the RAM that is really free.
- **HammerDB TPROC-H with `pg_verbose true`** prints every result row at a
  few hundred lines per second. Queries that return many rows (Q3, Q10, Q16)
  then take minutes of printing that HammerDB counts as query time: 2,788 s
  for Q10 instead of about 1 s. Use `pg_verbose false` for timing.
- **HammerDB's 1% New-Order rollback** surfaces as `ERROR: invalid transaction
  termination` when its `neword` function is called from another client,
  because the function ends with `ROLLBACK`. HammerDB itself logs it as
  "New Order Procedure Error". It is the designed rollback, not a problem.

## Reproducing

The scripts used for these runs are not part of the repository; they are
plain shell loops around the commands above. The shape of a comparison is:

1. Start the server as shown in "Setup".
2. Load the reference tool's schema and pgmeter's (`pgmeter init --spec
   samples/pgm-c -s 10 -j 4`) into two databases on that server.
3. `CHECKPOINT`, run the reference tool, `CHECKPOINT`, run pgmeter, and repeat
   the pair at least once. Discard the first run after each load.
4. Compare the reference tool's headline number with pgmeter's `--format json`
   output (`transactions[].throughput` and `latency_ms`).
