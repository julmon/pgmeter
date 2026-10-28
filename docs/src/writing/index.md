# Writing benchmarks

pgmeter runs a complete benchmark from a YAML specification file: the schema,
the data loading and the transaction workload, with Lua for any logic that is
too dynamic to express declaratively. Adding a benchmark never means modifying
or recompiling pgmeter.

This section is the complete reference for benchmark authors. It covers every
field of the spec file, the row-template mini-language, the binding mechanism,
and, in the [Lua API](../reference/lua-api.md) page, the full scripting API.

> The bundled PGM-B, PGM-C, PGM-IoT, PGM-H and PGM-DS workloads, derived from
> the TPC-B, TPC-C, TPCx-IoT, TPC-H and TPC-DS specifications, are all expressed
> as specs of this kind; `pgmeter spec new <name> -o .` writes a copy to read
> alongside these pages. (They are not audited, full-specification TPC
> benchmarks; see the [trademark notice](../project/license.md).)

## Anatomy of a spec file

A spec is a single YAML document with these top-level keys:

```yaml
name:               # string  (required) - benchmark name shown in output
description:        # string  (optional)

scaling:            # required - scale factor unit, max-id query, constants
metrics:            # optional - throughput unit and report layout
schema:             # required - tables, foreign_keys, indexes, pre/post DDL

# --- choose ONE data-loading style ---
load:               # list    - declarative, template-based COPY loaders
pre_load_script:    # string  - Lua run once before per-id loading
load_script:        # string  - Lua run per parallel job for loading

transactions:       # required - the workload (one or more transactions)

scripts:            # optional - shared Lua modules preloaded into every VM
```

The pages that follow describe each of them, except `metrics`, described
below.

## `metrics`: how a run is reported

```yaml
metrics:
  throughput_unit: "queries/hr"   # label of the throughput figures (default TPM)
  per: hour                       # second, minute (default) or hour
  report: stream                  # throughput (default) or stream
```

`throughput_unit` is only a label; `per` sets the time base the committed
count is turned into a rate with. `report` picks the layout of the results:

- `throughput` judges the run by its rate over the measured window: the
  headline throughput and percentiles, and whether the rate held steady.
  This suits OLTP workloads such as PGM-B and PGM-C.
- `stream` judges the run by where its time went, transaction by
  transaction: the elapsed time, the geometric mean of the response times,
  each transaction's share of the time and a timeline per client. This
  suits analytical workloads such as PGM-H and PGM-DS.

`run --once` always reports a stream. [Reading the
results](../guide/results.md) shows both layouts.

## A minimal spec

Create `my-bench.yaml`:

```yaml
name: hello-bench
description: Smallest possible benchmark

scaling:
  unit: account
  max_id_query: "SELECT MAX(id) FROM account"

schema:
  tables:
    - name: account
      columns:
        - { name: id,      type: "INTEGER NOT NULL" }
        - { name: balance, type: "INTEGER" }
      primary_key: [id]

load:
  - table: account
    copy_columns: [id, balance]
    rows_per_id: 1
    row: "{{id}}\t0\n"

transactions:
  - id: 1
    name: read-account
    weight: 100
    mode: simple
    bindings:
      acct: "rand_int(min_id, max_id)"
    sql: "SELECT balance FROM account WHERE id = {{acct}};"
```

Initialize and run it:

```bash
# load 1000 accounts (scale factor 1000)
pgmeter -H localhost -d benchmark init --spec my-bench.yaml -s 1000 -j 4

# run 8 clients for 60s with a 10s ramp-up
pgmeter -H localhost -d benchmark run --spec my-bench.yaml -c 8 -T 60 -r 10
```

That is the whole loop: define the schema, how to fill it and what to run,
then `init` and `run`. Everything else in this section is detail on those
three parts.
