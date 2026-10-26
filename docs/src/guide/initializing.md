# Initializing a benchmark

`init` creates the schema a spec declares and loads its data:

```bash
pgmeter [connection options] init --spec <DIR|FILE> [-s N] [-j N] [--no-fkey] [--post-create-table FILE] [-q]
```

> **`init` is destructive.** It runs `DROP TABLE IF EXISTS <table> CASCADE`
> for every table in the spec's `schema.tables` before creating it, then
> loads the data. Views, foreign keys from other tables and anything else
> that depends on those tables go with them. Point it at a database you can
> afford to wipe.

| Option | Default | Description |
|---|---|---|
| `--spec` | required | A benchmark directory containing `spec.yaml` (what `spec new` writes), or a single spec file |
| `-s, --scalefactor` | 1 | How many scale units to load; the unit is defined by the spec (`branch` for PGM-B, `warehouse` for PGM-C, ...) |
| `-j, --jobs` | 1 | Parallel loader connections; the unit ids are distributed round-robin across them |
| `--no-fkey` | off | Skip the foreign keys |
| `--post-create-table` | none | An SQL file to run after the tables are created, before loading |
| `-q, --quiet` | off | No progress output |

## What happens, in order

1. Schema: the spec's `pre_ddls`, then `DROP TABLE` and `CREATE TABLE` for
   every table, in one transaction, then the `--post-create-table` file if
   given.
2. Pre-loading: the spec's `pre_load_script`, if any, on one connection. The
   bundled workloads use it for their small reference tables.
3. Loading: `-j` jobs, each loading its share of the `1..N` unit ids with
   `COPY`, from either the spec's row templates or its Lua loader.
4. Primary keys, then foreign keys (unless `--no-fkey`), then indexes. They
   are created after the load, which is much faster than loading into indexed
   tables.
5. `VACUUM FREEZE ANALYZE` on every table, then the spec's `post_ddls`.
6. A `CHECKPOINT`, so the run that follows does not pay for the load.

Each step prints its duration. [The init lifecycle](../writing/init-lifecycle.md)
describes the same steps from the spec author's side.

## Choosing a scale factor

The scale factor sets the data size and the key space the workload spreads
over, so it is part of what you compare. Two runs are comparable only at the
same scale factor. The bundled workloads document their data volume per unit
in [Bundled workloads](workloads.md); as a rule of thumb, pick a scale factor
whose data is larger than the server's `shared_buffers` if you want to
exercise storage, and smaller if you want a CPU-bound comparison.

## Reloading between runs

A run modifies the data (PGM-B and PGM-C insert history and orders; PGM-C's
new orders grow the order tables). Running `init` again before each
measurement puts every run on the same starting point. The first run after
a load is usually a little slower than the next ones, as autovacuum and
autoanalyze settle; treat it as a warm-up.
