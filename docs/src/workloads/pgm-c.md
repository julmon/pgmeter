# PGM-C

An order-entry workload derived from TPC-C: five transactions over
warehouses, districts, customers, orders and stock, with the mix, the input
distributions and the designed 1% rollback the TPC-C text prescribes. Each
transaction runs as one PL/pgSQL function call, which is also how HammerDB's
TPROC-C drives PostgreSQL. The metric is committed transactions per minute.

[PGM-C-client](pgm-c-client.md) is the same workload driven statement by
statement from the client.

```bash
pgmeter spec new pgm-c -o .
pgmeter -d benchmark init --spec pgm-c -s 10 -j 4
pgmeter -d benchmark run  --spec pgm-c -c 8 -T 120 -r 60
```

## Scale and data

The scale unit is a **warehouse**. Per warehouse:

| Table | Rows per warehouse | Notes |
|---|---|---|
| `warehouse` | 1 | |
| `district` | 10 | next order id per district |
| `customer` | 30,000 | 3,000 per district; last names from the TPC-C syllable scheme, the first 1,000 per district covering every name, the rest by the load-time non-uniform distribution; 10% have bad credit |
| `history` | 30,000 | one initial payment per customer |
| `orders` | 30,000 | 3,000 per district |
| `order_line` | about 300,000 | 5 to 15 lines per order |
| `new_order` | 9,000 | the last 900 orders of each district are undelivered |
| `stock` | 100,000 | one row per item |

`item` has 100,000 rows shared by every warehouse, loaded once before the
warehouses. At 10 warehouses the tables take about 1 GB and the indexes
200 MB more. Primary keys
on every table plus the two TPC-C secondary indexes, `customer
(c_w_id, c_d_id, c_last, c_first, c_id)` for the by-name lookups and
`orders (o_w_id, o_d_id, o_c_id, o_id)` for Order-Status. There are no
foreign keys and no other indexes, on purpose: HammerDB's schema declares
neither, TPC-C does not require them, and they slowed Stock-Level and
New-Order considerably in testing. Add them to the spec if your scenario
has them.

## The transactions

| Name | Weight | What it does |
|---|---|---|
| New-Order | 45 | One order of 5 to 15 lines for a customer of one district: increments the district's order counter, inserts the order, its new-order entry and its lines, updates the stock rows. 1% of the lines are supplied by another warehouse. 1% of the orders ask for an item that does not exist and roll back |
| Payment | 43 | Records a payment of 1 to 5,000 against a customer, a district and a warehouse; 15% of the customers belong to another warehouse; 60% are looked up by last name (the middle customer of that name) rather than by id; bad-credit customers get their history string rewritten |
| Order-Status | 4 | A customer's last order and its lines; 60% by last name |
| Delivery | 4 | Delivers the oldest undelivered order of each of the 10 districts of a warehouse: removes the new-order entries, sets the carrier, stamps the lines, credits the customers |
| Stock-Level | 4 | Counts the distinct items below a threshold of 10 to 20 among the last 20 orders of a district |

Customers, items and last names are drawn with TPC-C's non-uniform random
function (NURand), so a few hot rows take most of the traffic as the
specification intends. Each client picks the next transaction at random by
weight, with no think time.

Each transaction is a `SELECT pgmc_<name>(...)` round trip; the five
functions are created by the spec's `post_ddls` at `init` time, in plain
PL/pgSQL you can read and change in the spec. Delivery visits the districts
in order and New-Order updates its stock rows sorted by key, so concurrent
transactions take their row locks in the same order and do not deadlock.

## Errors you should expect

A correct run reports errors on New-Order only, about 1% of them, with the
message `New-Order rollback: unknown item` in `error.log`. That is the
rollback TPC-C requires, not a failure. Any other message is a real problem.

## Deviations from TPC-C

- No terminals, no keying and think times: every client runs transactions
  back to back, so the throughput per warehouse is far above TPC-C's
  12.86 tpmC ceiling, and the warehouse count sets the data size rather than
  the load.
- Delivery runs inline as a normal transaction rather than through the
  deferred queue the specification describes.
- The number of lines per order follows a deterministic formula over the
  order id rather than a uniform draw, which gives the same 5 to 15 range
  and about the same average.
- The headline figure is committed transactions per minute over all five
  transactions. The results table has the committed New-Orders per minute
  for anyone who wants the TPC-C-style view; neither is tpmC.
