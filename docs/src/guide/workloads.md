# Bundled workloads

Seven specs are embedded in the binary: six workloads and a template. `pgmeter spec list` prints them;
`pgmeter spec new <name> -o <dir>` writes an editable copy, which is what
`init` and `run` take:

```bash
pgmeter spec list
pgmeter spec new pgm-c -o .          # creates ./pgm-c/spec.yaml and ./pgm-c/scripts/
pgmeter -d benchmark init --spec pgm-c -s 10 -j 4
pgmeter -d benchmark run  --spec pgm-c -c 8 -T 120 -r 30
```

`spec new` refuses to overwrite an existing directory unless `--force` is
given. The copy is yours to edit: change the constants, the weights, the
indexes or the SQL, and the same `init` and `run` apply.

| Name | Workload | Kind | Derived from | Scale unit |
|---|---|---|---|---|
| `pgm-b` | Banking debit/credit: one transaction updating an account, a teller and a branch and inserting a history row | OLTP | TPC-B | branch (10 tellers, 100,000 accounts) |
| `pgm-c` | Order entry: New-Order, Payment, Order-Status, Delivery and Stock-Level, run as PL/pgSQL functions, one round trip per transaction, the model HammerDB's TPROC-C uses | OLTP | TPC-C | warehouse (10 districts, 30,000 customers, 100,000 stock rows) |
| `pgm-c-client` | The same order entry driven from the client in Lua, one statement per round trip: an application without stored procedures | OLTP | TPC-C | warehouse |
| `pgm-h` | Decision support: 22 analytical queries with random parameters over an 8-table schema | OLAP | TPC-H | scale factor (1.5 million orders, 6 million order lines) |
| `pgm-ds` | Decision support over a 24-table snowflake schema | OLAP | TPC-DS | scale factor |
| `pgm-iot` | IoT gateway: data ingestion, scan, device and alert queries, threshold updates | Time series | TPCx-IoT | substation |
| `custom-simple` | A minimal user-defined workload: tenants, products, orders, three transactions | OLTP | none | tenant |

Things to know before comparing numbers:

- The workloads are **derived from** the TPC specifications: they implement a
  representative subset of each, for relative comparison (two configurations,
  two servers, two table access methods under the same load). They are not
  audited, full-specification TPC benchmarks and their results must not be
  compared with or presented as official TPC results. See
  [License and trademarks](../project/license.md).
- PGM-C rolls back about 1% of its New-Orders on purpose, as TPC-C prescribes.
  They are reported as errors with the message
  `New-Order rollback: unknown item`; other messages in `error.log` are real
  failures.
- PGM-C and PGM-C-client load the same data and run the same mix. PGM-C puts
  the whole transaction in a server-side function, which is how HammerDB
  drives PostgreSQL; PGM-C-client sends every statement from the client,
  which is how most applications behave. Their throughput differs by the
  round trips, not by the work the server does.
- PGM-H and PGM-DS are single-stream analytical workloads: run them with one
  client and read the per-query response times, not the throughput.
- Customers, items and last names in the two PGM-C variants follow TPC-C's
  non-uniform random distribution, so the hot spots are the ones the
  specification intends.

Each workload has a reference page with its data volumes, its transactions
and the deviations from the TPC text: [PGM-B](../workloads/pgm-b.md),
[PGM-C](../workloads/pgm-c.md), [PGM-C-client](../workloads/pgm-c-client.md),
[PGM-H](../workloads/pgm-h.md), [PGM-DS](../workloads/pgm-ds.md) and
[PGM-IoT](../workloads/pgm-iot.md).
