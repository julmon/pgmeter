# PGM-B

A banking debit/credit workload derived from TPC-B: one transaction that
updates an account, a teller and a branch and records the operation in a
history table. It is the simplest bundled workload and the one to use for
raw OLTP throughput of small write transactions. Its metric is transactions
per second.

```bash
pgmeter spec new pgm-b -o .
pgmeter -d benchmark init --spec pgm-b -s 10 -j 4
pgmeter -d benchmark run  --spec pgm-b -c 8 -T 60 -r 10
```

## Scale and data

The scale unit is a **branch**. Per branch:

| Table | Rows per branch | Notes |
|---|---|---|
| `branch` | 1 | balance, filler |
| `teller` | 10 | balance, branch id, filler |
| `account` | 100,000 | balance, branch id, filler |
| `history` | 0, grows by one row per transaction | account, teller, branch, delta, timestamp |

At scale factor 10 that is one million accounts. Primary keys on the three
id columns, an index on `branch_id` in
`teller` and `account`, and foreign keys from `teller` and `account` to
`branch` (skipped with `--no-fkey`). The filler columns pad the rows to the
widths the TPC-B text describes.

## The transaction

| Name | Weight | What it does |
|---|---|---|
| Debit/credit | 100 | `BEGIN`; add a random delta in [-5000, 5000] to one account; read that account's balance; add the delta to one teller and to one branch; insert a history row; `COMMIT` |

The seven statements are sent one at a time, one round trip each, which is
what pgbench's built-in script does, so the two report comparable rates on
the same server. The account, teller and branch are drawn uniformly over the
whole loaded range, independently of each other.

## Deviations from TPC-B

- No terminals and no think time: every client runs transactions back to
  back.
- The account and teller are not chosen within the transaction's branch
  (TPC-B keeps 85% of accounts local to the branch); they are uniform over
  the whole dataset, like pgbench.
- The headline figure is committed transactions per second over the measured
  window, not the TPC-B tpsB metric with its residence-time constraints.

The spec is a short YAML file with no Lua, and a good first spec to read.
