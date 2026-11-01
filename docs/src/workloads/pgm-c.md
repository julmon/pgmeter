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
on every table but `history`, which TPC-C leaves without a key, plus the two
TPC-C secondary indexes, `customer
(c_w_id, c_d_id, c_last, c_first, c_id)` for the by-name lookups and
`orders (o_w_id, o_d_id, o_c_id, o_id)` for Order-Status, and the ten
foreign keys of TPC-C's referential integrity. The foreign keys cost every
New-Order about a dozen extra index probes; HammerDB's TPROC-C schema
declares none, so `pgmeter init --no-fkey` is the setting to compare with
it. There are no other indexes: an extra one on stock quantities turned
Stock-Level into a hash join over every low-stock item of the warehouse and
was removed.

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
in order and locks each district's oldest order as it reads it, skipping one
another delivery holds, and New-Order feeds its stock update from the lines
sorted by key, so concurrent transactions take their row locks in the same
order. No run has deadlocked so far; the sort is an `ORDER BY` in a CTE,
which the planner has honoured in every plan seen, not a guarantee the
server gives.

## Errors you should expect

A correct run reports errors on New-Order only, about 1% of them, with the
message `New-Order rollback: unknown item` in `error.log`. That is the
rollback TPC-C requires, not a failure. Any other message is a real problem.

## Deviations from TPC-C

- No terminals, no keying and think times: every client runs transactions
  back to back, so the throughput per warehouse is far above TPC-C's
  12.86 tpmC ceiling, and the warehouse count sets the data size rather than
  the load.
- Every transaction draws its warehouse and district at random. TPC-C
  attaches each terminal to one district and has it emulate a user there;
  the hot rows and the remote shares are the same, the per-terminal
  bookkeeping is not.
- Delivery runs inline as a normal transaction rather than through the
  deferred queue the specification describes, and the districts it skipped
  are counted, not reported.
- The reads a transaction makes only to display them are left out: New-Order
  does not fetch the item names nor compare the `ORIGINAL` markers of the item
  and stock data, Payment does not read the warehouse and district addresses,
  Order-Status does not read the customer's name and balance. Stock-Level is
  the two statements the specification describes.
- The items of an order are distinct. TPC-C draws each line's item on its
  own, so an order may name the same item twice; one set-based stock update
  per order needs them distinct.
- The constants of the non-uniform random function are fixed in
  `pgmc_common.lua` rather than drawn once per database, so every load is the
  same; the load-time and run-time constants for last names differ by 88,
  within the 65 to 119 the specification requires.
- The headline figure is committed transactions per minute over all five
  transactions. The results table has the committed New-Orders per minute
  for anyone who wants the TPC-C-style view; neither is tpmC.
