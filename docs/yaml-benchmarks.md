# Writing Benchmarks in YAML + Lua

pgmeter can run a complete benchmark — schema, data loading, and the transaction
workload — entirely from a YAML specification file, with Lua for any logic that
is too dynamic to express declaratively. You do **not** need to modify or
recompile pgmeter to add a new benchmark.

This document is the complete reference for benchmark authors. It covers every
field of the spec file, the row-template mini-language, the binding mechanism,
and the full Lua scripting API.

> The built-in PGM-B, PGM-C, PGM-IoT, PGM-H, and PGM-DS workloads — derived from
> the TPC-B/C, TPCx-IoT and TPC-H/DS specifications — are all expressed as YAML
> specs under [`samples/`](../samples). They are the best worked examples to read
> alongside this guide. (They are not audited, full-specification TPC benchmarks;
> see the trademark notice in the project README.)

---

## Table of contents

1. [Quick start](#1-quick-start)
2. [Command-line usage](#2-command-line-usage)
3. [Anatomy of a spec file](#3-anatomy-of-a-spec-file)
4. [`scaling` — scale factor and context](#4-scaling--scale-factor-and-context)
5. [`schema` — tables, keys, indexes](#5-schema--tables-keys-indexes)
6. [Data loading](#6-data-loading)
   - [Template-based loading (`load:`)](#61-template-based-loading-load)
   - [Lua-based loading (`load_script` / `pre_load_script`)](#62-lua-based-loading-load_script--pre_load_script)
7. [`transactions` — the workload](#7-transactions--the-workload)
   - [`simple` mode](#71-simple-mode)
   - [`script` mode](#72-script-mode)
8. [`scripts.modules` — shared Lua code](#8-scriptsmodules--shared-lua-code)
9. [The Lua API reference](#9-the-lua-api-reference)
10. [The init lifecycle](#10-the-init-lifecycle)
11. [Worked example: build a benchmark from scratch](#11-worked-example-build-a-benchmark-from-scratch)
12. [Performance tips](#12-performance-tips)
13. [Limitations and gotchas](#13-limitations-and-gotchas)
14. [Cheat sheets](#14-cheat-sheets)

---

## 1. Quick start

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

That is the whole loop: define schema + how to fill it + what queries to run,
then `init` and `run`.

---

## 2. Command-line usage

A benchmark is driven by the `init` and `run` subcommands, each taking the spec
via `--spec`. The connection options are shared with all other pgmeter commands.

`--spec` accepts either a benchmark **directory** (containing a `spec.yaml`, like
the bundled workloads under `samples/`) or a single spec file.

### Connection options (global, before the subcommand)

| Option | Env var | Default | Description |
|--------|---------|---------|-------------|
| `-H, --host` | `PGHOST` | localhost | Host or socket directory |
| `-p, --port` | `PGPORT` | 5432 | Port |
| `-U, --username` | `PGUSER` | current user | Database user |
| `-d, --dbname` | `PGDATABASE` | current user | Database name |

The password is taken from `PGPASSWORD`.

### `init`

```bash
pgmeter [conn opts] init --spec <FILE> [-s <N>] [-j <N>] [--no-fkey] [--post-create-table <FILE>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--spec` | (required) | Path to the YAML spec |
| `-s, --scalefactor` | 1 | Number of scale-factor units to generate (e.g. 1000 accounts) |
| `-j, --jobs` | 1 | Parallel data-loading threads |
| `--no-fkey` | false | Skip foreign-key creation |
| `--post-create-table` | — | Extra SQL file executed after table creation |

### `run`

```bash
pgmeter [conn opts] run --spec <FILE> [-c <N>] [-T <N>] [-r <N>] [-P <N>] [--format <FMT>] [-q]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--spec` | (required) | Path to the YAML spec |
| `-c, --client` | 1 | Concurrent clients |
| `-T, --time` | 1 | Benchmark duration (seconds) |
| `-r, --rampup` | 0 | Ramp-up period (clients start gradually) |
| `-P, --progress` | 0 | Print live progress every N seconds (0 disables) |
| `--format` | table | Results output format: `table`, `json`, or `csv` |
| `-q, --quiet` | false | Suppress progress output; print only result data |

> **Use the same `--spec` file for `init` and `run`.** The spec is the single
> source of truth for both the schema and the workload.

---

## 3. Anatomy of a spec file

A spec is a single YAML document with these top-level keys:

```yaml
name:               # string  (required) - benchmark name shown in output
description:        # string  (optional)

scaling:            # required - scale factor unit, max-id query, constants
schema:             # required - tables, foreign_keys, indexes, pre/post DDL

# --- choose ONE data-loading style ---
load:               # list    - declarative, template-based COPY loaders
pre_load_script:    # string  - Lua run once before per-id loading
load_script:        # string  - Lua run per parallel job for loading

transactions:       # required - the workload (one or more transactions)

scripts:            # optional - shared Lua modules preloaded into every VM
```

Each is described in detail below.

---

## 4. `scaling` — scale factor and context

```yaml
scaling:
  unit: branch
  max_id_query: "SELECT MAX(branch_id) AS max_id FROM branch"
  constants:
    tellers_per_branch: 10
    accounts_per_branch: 100000
```

| Field | Required | Meaning |
|-------|----------|---------|
| `unit` | no | A human label for the scale-factor entity (e.g. `branch`, `warehouse`). **Descriptive only** — it is not used by the engine. |
| `max_id_query` | yes | SQL returning a single integer: the largest scale-unit id present. Run once at the start of each `run` to set `max_id`. Must return an `int`. |
| `constants` | no | Scalar values (number / string / bool) exposed to expressions and scripts as `const.<name>`. |

### The scale-factor model

pgmeter loads `-s N` **units**, numbered `1..N`. The unit id is the partitioning
key for everything:

- During loading, each unit id (`id`) is processed once, and you generate the
  rows that belong to it.
- During a run, every transaction picks a random unit id in `[min_id, max_id]`
  and operates on that unit's data.

This is what makes data generation and the workload scale together: at scale
factor 1000 you get 1000 branches, and the workload spreads across branches
`1..1000`.

### Context variables

The following are injected into the Lua environment used for bindings, row
templates, and scripts:

| Variable | Available in | Value |
|----------|-------------|-------|
| `min_id` | bindings, templates, scripts | always `1` |
| `max_id` | bindings, templates, scripts | auto-detected at the start of each run via `max_id_query` |
| `scalefactor` | **loading only** | from `-s` at init time. **`0` during a run** — do not rely on it in transactions. |
| `const.<name>` | everywhere | values from `scaling.constants` |
| `ids` | load scripts only | array of unit ids assigned to the current job |
| `i` | row templates only | row counter, `1..rows_per_id` |
| `id` | row templates only | current unit id |

> ⚠️ **`scalefactor` is `0` while a benchmark runs.** It only carries the real
> value during `init`/loading. In transaction logic, derive ranges from
> `min_id`, `max_id`, and `const.*` instead.

`constants` must be scalars (numbers, strings, booleans). Nested lists/maps are
ignored when injected into Lua.

---

## 5. `schema` — tables, keys, indexes

```yaml
schema:
  pre_ddls:            # optional - SQL run before any CREATE TABLE
    - "CREATE SEQUENCE measurement_id_seq"
  tables:
    - name: account
      columns:
        - { name: id,      type: "INTEGER NOT NULL" }
        - { name: balance, type: "INTEGER" }
        - { name: filler,  type: "CHAR(84)" }
      primary_key: [id]
      access_method: heap          # optional
      storage:                     # optional - WITH (...) options
        fillfactor: 90
        autovacuum_enabled: false
  foreign_keys:
    - name: fk_x
      table: child
      columns: [parent_id]
      references: { table: parent, columns: [id] }
  indexes:
    - { name: idx_balance, table: account, columns: [balance] }
  post_ddls:           # optional - SQL appended to the vacuum step
    - "SELECT setval('measurement_id_seq', (SELECT MAX(id) FROM measurement))"
```

### `tables`

| Field | Required | Maps to |
|-------|----------|---------|
| `name` | yes | table name |
| `columns` | yes | list of `{ name, type }`. `type` is raw SQL — it can include `NOT NULL`, `DEFAULT ...`, `SERIAL`, etc. |
| `primary_key` | no | `ALTER TABLE ... ADD PRIMARY KEY (...)` (created after loading) |
| `access_method` | no | `CREATE TABLE ... USING <method>` (e.g. `heap`, `columnar`) |
| `storage` | no | `CREATE TABLE ... WITH (k = v, ...)` |

Each table is generated as `DROP TABLE IF EXISTS <name> CASCADE` followed by
`CREATE TABLE`, so re-running `init` is idempotent.

> **Column `type` is passed through verbatim.** Whatever you write lands in the
> `CREATE TABLE` between the column name and the next comma. This is the escape
> hatch for defaults, generated columns, check constraints on a column, etc.

### `foreign_keys`

Emitted as `ALTER TABLE <table> ADD CONSTRAINT <name> FOREIGN KEY (<columns>)
REFERENCES <references.table> (<references.columns>)`, created **after** data is
loaded. Skipped entirely when `init` is run with `--no-fkey`.

### `indexes`

```yaml
indexes:
  - { name: i_ts,     table: measurement, columns: [ts] }
  - { name: i_uniq,   table: account,     columns: [email], unique: true }
  - { name: i_brin,   table: measurement, columns: [ts], type: brin }
  - { name: i_cover,  table: measurement, columns: [ss_id, ts], include: [sn_id, value] }
  - { name: i_unack,  table: alert,       columns: [ss_id], where: "NOT acknowledged" }
```

| Field | Required | Maps to |
|-------|----------|---------|
| `name` | yes | index name |
| `table` | yes | target table |
| `columns` | yes | key columns |
| `unique` | no | `CREATE UNIQUE INDEX` |
| `type` | no | `USING <type>` — `btree` (default), `hash`, `gin`, `gist`, `brin`, ... |
| `include` | no | `INCLUDE (<cols>)` — covering / non-key columns |
| `where` | no | `WHERE <predicate>` — partial index |

The generated statement is:

```
CREATE [UNIQUE] INDEX <name> ON <table> [USING <type>] (<columns>) [INCLUDE (<include>)] [WHERE <where>]
```

> For a benchmarking tool, index shape directly affects the numbers you measure.
> `type`, `include`, and `where` exist so the YAML can reproduce the exact index
> definitions a hand-tuned schema would use.

### `pre_ddls` / `post_ddls`

Free-form SQL statements. `pre_ddls` run before table creation (sequences,
`CREATE EXTENSION`, etc.). `post_ddls` run at the end of the vacuum step (e.g.
`setval` to align a sequence with loaded data).

---

## 6. Data loading

There are two mutually exclusive styles. Pick the one that fits:

| Style | Use when | Key |
|-------|----------|-----|
| **Template** | Rows are a simple function of `id` and a row counter `i`. | `load:` |
| **Lua script** | Loading needs control flow, lookups, cross-table coordination, or shared reference tables. | `load_script:` (+ optional `pre_load_script:`) |

If `load_script` is present it takes precedence and `load:` is ignored.

### 6.1 Template-based loading (`load:`)

Each entry is a loader for one table, executed once per unit id via PostgreSQL
`COPY`.

```yaml
load:
  - table: account
    method: copy                 # only "copy" is supported (default)
    copy_columns: [id, branch_id, balance, filler]
    rows_per_id: "{{const.accounts_per_branch}}"
    batch_size: 10000            # rows per COPY chunk (default 10000)
    row: "{{(id - 1) * const.accounts_per_branch + i}}\t{{id}}\t0\t\n"
```

| Field | Required | Meaning |
|-------|----------|---------|
| `table` | yes | target table |
| `method` | no | only `copy` is implemented |
| `copy_columns` | yes | column order for `COPY <table> (<copy_columns>) FROM stdin` |
| `rows_per_id` | yes | rows to generate per unit. Integer, or a `{{expr}}` string evaluated once (may use `const.*`). |
| `batch_size` | no | rows per `COPY` round-trip (default 10000) |
| `row` | yes | the **row template** (see below) |

#### The row-template mini-language

A `row` string is literal text with `{{ lua_expression }}` holes. The engine
compiles it once into a Lua function and calls it in batches.

- Output must be **tab-separated** (`\t`) values in `copy_columns` order, ending
  with a newline (`\n`) — this is PostgreSQL's default `COPY` text format.
- `{{expr}}` is replaced by `tostring(expr)`.
- Two loop variables are available:
  - `id` — the current unit id.
  - `i` — the row counter, running `1..rows_per_id`.
- `rand_int`, `rand_float`, `min_id`, `max_id`, `scalefactor`, and `const.*` are
  all in scope. Database functions (`query`, `exec`, ...) are **not** — template
  loading is pure generation.

Examples:

```yaml
# one row per unit: "<id>\t0\n"
row: "{{id}}\t0\n"

# N rows per unit with a globally unique id, random price and stock
row: "{{(id - 1) * 100 + i}}\t{{id}}\tProduct-{{i}}\t{{rand_float(1.0, 999.99)}}\t{{rand_int(0, 10000)}}\n"
```

#### NULLs and empty values in templates

The default `COPY` text format treats an empty field as an empty string and the
two-character sequence `\N` as SQL `NULL`. `CHAR(n)` columns left empty (as in
the PGM-B `filler` columns) are stored blank-padded. If you need a real `NULL`,
emit `\N`, or switch to a Lua loader where you control the `COPY` options.

### 6.2 Lua-based loading (`load_script` / `pre_load_script`)

For anything beyond a per-row formula, use Lua. Both fields accept either an
**inline script** (a multi-line YAML block) or a **path to a `.lua` file**
(resolved relative to the spec file). The rule: if the value contains a newline
it is treated as inline code, otherwise as a file path.

```yaml
pre_load_script: lua/setup_reference_tables.lua   # runs ONCE, before loading
load_script: lua/load_per_unit.lua                # runs per parallel job
```

- **`pre_load_script`** runs a single time on one connection before any per-unit
  loading. Use it to populate small shared/reference tables that are not
  partitioned by unit id (e.g. the PGM-C `item` table, PGM-H `region`/`nation`).
  It has the full DB API but `ids` is empty.

- **`load_script`** runs once per parallel job. The global `ids` is the array of
  unit ids assigned to *that* job (the engine round-robins the `1..scalefactor`
  ids across `-j` jobs). Your script loops over `ids` and loads each unit's data,
  typically via the `copy_*` streaming API:

```lua
for _, id in ipairs(ids) do
    copy_start("COPY account FROM stdin")
    for i = 1, const.accounts_per_unit do
        copy_write(string.format("%d\t%d\t0\n", (id - 1) * const.accounts_per_unit + i, id))
    end
    copy_end()
end
```

Both scripts have `scalefactor`, `const.*`, and the full Lua API (random, SQL,
and COPY helpers). `min_id`/`max_id` are **not** injected into loaders — iterate
`ids` instead.

> **Parallelism caveat:** because each job only sees its slice of `ids`, a
> `load_script` must compute any global offsets from the `id` value itself (as
> above with `(id - 1) * N + i`), never from a running counter. If a table is
> *not* partitioned by unit (a single shared table), load it from
> `pre_load_script`, or guard it with `if id == 1 then ... end` inside
> `load_script` so only one job creates it.

---

## 7. `transactions` — the workload

Each transaction is one weighted entry in the workload mix. On every step a
client picks a transaction at random, weighted by `weight`.

```yaml
transactions:
  - id: 1
    name: New-Order
    description: Create a customer order
    weight: 45
    mode: script          # "simple" | "script"
    # for simple mode:
    bindings: { ... }
    sql: "..."
    # for script mode (one of):
    script: "Q[1]()"          # inline Lua
    script_file: lua/new_order.lua   # external Lua file
```

| Field | Required | Meaning |
|-------|----------|---------|
| `id` | yes | unique integer id (used internally to dispatch) |
| `name` | yes | shown in the results table |
| `weight` | yes | relative selection weight (the mix is `weight / Σweight`) |
| `description` | no | free text |
| `mode` | no | `simple` (default) or `script` |
| `bindings` | simple | map of variable → Lua expression |
| `sql` | simple | SQL template with `{{var}}` holes |
| `script` | script | inline Lua source |
| `script_file` | script | path to a `.lua` file (relative to the spec) |

> Only **`simple`** and **`script`** modes exist. (Some sample comments mention
> an `extended` mode — it is not implemented and will error.)

### 7.1 `simple` mode

Best for fixed SQL with a few random parameters. The whole `sql` block is sent
to the server in **one round-trip** (`batch_execute`), so:

- You may include several statements separated by `;`.
- If you want them to be atomic, wrap them yourself in `BEGIN; ... COMMIT;`.
  Otherwise each statement autocommits.
- No result rows are returned to the client (this mode is fire-and-measure).

**Bindings** are evaluated just before the SQL is rendered. Each binding value
is a Lua expression evaluated against the context (`min_id`, `max_id`,
`scalefactor`, `const.*`, plus `rand_int`/`rand_float`). The result substitutes
every `{{name}}` occurrence in `sql`.

```yaml
- id: 1
  name: PGM-B
  weight: 100
  mode: simple
  bindings:
    branch_id:  "rand_int(min_id, max_id)"
    account_id: "rand_int((min_id - 1) * const.accounts_per_branch + 1, max_id * const.accounts_per_branch)"
    delta:      "rand_int(-5000, 5000)"
  sql: |
    BEGIN;
    UPDATE account SET account_balance = account_balance + {{delta}} WHERE account_id = {{account_id}};
    UPDATE branch  SET branch_balance  = branch_balance  + {{delta}} WHERE branch_id  = {{branch_id}};
    INSERT INTO history (account_id, branch_id, delta, ts)
      VALUES ({{account_id}}, {{branch_id}}, {{delta}}, CURRENT_TIMESTAMP);
    COMMIT;
```

Binding values are substituted as literals: integers and floats verbatim,
strings single-quoted and escaped, booleans as `true`/`false`, and `nil` as
`NULL`.

> **Bindings evaluate in declaration order.** Each binding becomes a `local` in a
> single generated Lua function, in the order written, so a later binding may
> reference an earlier one (e.g. `account_id` can use `branch_id`). For anything
> beyond a one-line derivation — reads that feed writes, branching, loops — use
> `script` mode instead. See [Limitations](#13-limitations-and-gotchas).

### 7.2 `script` mode

Best for multi-statement transactions with control flow, reads that feed writes,
conditional rollback, etc. The Lua source (inline `script` or `script_file`)
becomes the transaction body and runs with the full [Lua API](#9-the-lua-api-reference).

You manage the transaction boundary explicitly with `begin()` / `commit()` /
`rollback()`, and use `query`/`query_one`/`exec` for SQL.

```lua
-- new_order.lua
local w_id = rand_int(min_id, max_id)
local d_id = rand_int(1, 10)

begin()

local d = query(
  "UPDATE district SET d_next_o_id = d_next_o_id + 1 WHERE d_w_id = $1 AND d_id = $2 RETURNING d_next_o_id",
  w_id, d_id)
local o_id = d[1]["d_next_o_id"] - 1

exec("INSERT INTO orders (o_id, o_d_id, o_w_id, o_entry_d) VALUES ($1, $2, $3, NOW())",
     o_id, d_id, w_id)

commit()
```

A common pattern is a **dispatch table** loaded via `scripts.modules`, with each
transaction's `script` just calling into it:

```yaml
scripts:
  modules: [lua/queries.lua]     # defines a global table Q with Q[1]..Q[22]
transactions:
  - { id: 1, name: Q1, weight: 8, mode: script, script: "Q[1]()" }
  - { id: 2, name: Q2, weight: 4, mode: script, script: "Q[2]()" }
```

---

## 8. `scripts.modules` — shared Lua code

```yaml
scripts:
  modules:
    - lua/helpers.lua
    - lua/queries.lua
```

Each listed file is loaded **once per worker thread**, at global scope, before
any transaction runs. Use modules for:

- Helper functions shared across transactions.
- Dispatch tables (e.g. `Q[1]..Q[22]` for PGM-H).
- Constant lookup tables (syllable lists, type names, ...).

Anything a module defines as a global is visible to every `script`-mode
transaction on that thread.

---

## 9. The Lua API reference

### 9.1 Execution model

- Each benchmark **worker thread** owns one Lua VM, created lazily on its first
  `script`-mode transaction. The VM lives for the whole run.
- On VM creation: context globals are injected, then `scripts.modules` are
  executed in order, then every transaction `script` is compiled into a named
  function. Subsequent executions just call the precompiled function — there is
  no re-parsing per transaction.
- Because the VM persists, **globals set by one transaction survive into the
  next** on the same thread. This is useful for caching (compile a prepared
  string once), but means you should not assume a clean slate each call.
- Data-loading scripts (`pre_load_script` / `load_script`) get a **fresh VM per
  invocation**.

### 9.2 Globals available to scripts

| Global | In transactions | In loaders |
|--------|-----------------|-----------|
| `min_id`, `max_id` | yes | no |
| `scalefactor` | `0` (unused at run time) | yes |
| `const` (table) | yes | yes |
| `ids` (array) | no | yes (per-job slice) |

Plus the standard Luau library (`string`, `table`, `math`, `tostring`,
`ipairs`, ...). The engine uses **Luau** (via the `mlua` crate), so Luau
semantics apply (notably: **arrays/tables are 1-indexed**).

### 9.3 Random data functions

| Function | Returns | Notes |
|----------|---------|-------|
| `rand_int(min, max)` | integer | **Inclusive** on both ends: `[min, max]`. |
| `rand_float(min, max)` | number | Half-open: `[min, max)`. |
| `rand_string(min_len, max_len)` | string | Random alphanumeric, length in `[min_len, max_len]`. If `min_len >= max_len`, length is exactly `max_len`. |
| `rand_strings(count, min_len, max_len)` | array | `count` random alphanumeric strings in one call (batched — cheaper than calling `rand_string` in a loop during bulk loads). |

### 9.4 SQL execution

All SQL uses PostgreSQL's **simple query protocol**. Parameters are passed as
extra arguments after the SQL string and interpolated into `$1`, `$2`, … as
**literals** (not server-side bind parameters):

- integers / floats → written as-is
- strings → single-quoted, with `'` escaped to `''`
- booleans → `true` / `false`
- `nil` → `NULL`

| Function | Returns | Description |
|----------|---------|-------------|
| `exec(sql, ...params)` | nothing | Run a statement, ignore any result. |
| `query(sql, ...params)` | array of rows | Each row is a table keyed by **column name**. 1-indexed array. |
| `query_one(sql, ...params)` | scalar or `nil` | First column of the first row; `nil` if no rows. |

Result values come back through the simple protocol as **text, without type
information**, and are converted by shape, matching what PostgreSQL prints:

| Text looks like | Lua value |
|-----------------|-----------|
| `-?[0-9]+`, no leading zeros (`42`, `-7`, `0`) | integer; beyond the 32-bit range an exact number up to 2^53, above that the text is kept |
| decimal or exponent form (`3.5`, `-0.25`, `1e+20`, `1.5E-3`) | number |
| exactly `t` / `f` (the boolean output format) | boolean |
| anything else (`NaN`, `Infinity`, `007`, `true`, dates, timestamps, `numeric` with more than 53 bits) | string |

Because the shape is all there is, a **text column** whose value happens to be
`t`, `f` or a bare number is converted too. If that matters, keep the value
unambiguous on the SQL side (for example prefix it, `'#' || col`) or do the
comparison in SQL. So:

```lua
local rows = query("SELECT c_discount, c_last FROM customer WHERE c_id = $1", cid)
local discount = rows[1]["c_discount"]   -- number
local last     = rows[1]["c_last"]       -- string

local total = query_one("SELECT count(*) FROM orders WHERE o_c_id = $1", cid)  -- number or nil
```

> `$n` placeholders are substituted by simple text replacement (highest index
> first, so `$10` is handled before `$1`). Use them only as value placeholders.

### 9.5 Transaction control

| Function | SQL issued |
|----------|-----------|
| `begin()` | `BEGIN` |
| `commit()` | `COMMIT` |
| `rollback()` | `ROLLBACK` |

A `script` transaction that should be atomic must call `begin()` at the start
and `commit()` at the end. To abort, call `rollback()`.

### 9.6 Error handling

Raising a Lua error (via `error("...")`) aborts the current transaction; pgmeter
records it as an **error** in the results (the per-transaction error count and
rate). The standard pattern for an intentional abort is to roll back first, then
error:

```lua
local item = query("SELECT i_price FROM item WHERE i_id = $1", iid)
if #item == 0 then
    rollback()
    error("invalid item")   -- counted as an error / rollback in the report
end
```

This is how PGM-C mirrors the ~1% New-Order rollback rate mandated by the TPC-C
specification.

### 9.7 Bulk COPY streaming (loaders)

For high-throughput loading inside `load_script` / `pre_load_script`:

| Function | Description |
|----------|-------------|
| `copy_start(sql)` | Begin a `COPY <table> [(cols)] FROM stdin [options]` stream. You provide the full `COPY` statement, so you can set `NULL AS ''`, choose columns, etc. |
| `copy_write(data)` | Write a chunk of `COPY` text — typically one or more `\t`-separated, `\n`-terminated rows. |
| `copy_end()` | Finish/flush the stream. |

Only one COPY stream is active at a time per thread. Any stream left open when a
loader finishes is flushed automatically, but call `copy_end()` explicitly.

```lua
copy_start("COPY measurement FROM stdin")
for k = 1, n do
  copy_write(string.format("%d\t%d\t%.4f\n", id_base + k, sensor, rand_float(50, 150)))
end
copy_end()
```

---

## 10. The init lifecycle

`init` performs these steps in order (you can see each in the console
output):

1. **Database DDLs** — `pre_ddls`, then `DROP/CREATE TABLE` for every table
   (with `access_method` / `storage`). Run in one transaction.
2. **Pre-loading** — `pre_load_script`, if present (single connection).
3. **Data loading** — `-j` parallel jobs. The `1..scalefactor` unit ids are
   round-robin distributed across jobs; each job runs the template loaders or
   `load_script` for its ids.
4. **Primary keys** — `ALTER TABLE ... ADD PRIMARY KEY`.
5. **Foreign keys** — unless `--no-fkey`.
6. **Indexes** — all `indexes`.
7. **Vacuum** — `VACUUM FREEZE ANALYZE` per table, then `post_ddls`.
8. **Checkpoint** — forces a checkpoint so the run starts from a clean state.

Keys and indexes are created *after* loading for speed. This ordering is why
your `row` templates / `load_script` only need to produce raw column data — the
constraints are applied afterwards.

---

## 11. Worked example: build a benchmark from scratch

Let's build a small **link-shortener** benchmark from nothing. Scale unit = a
"workspace"; each workspace owns 1000 links. The workload is 80% redirects
(read a link, bump its hit counter) and 20% link creation.

### Step 1 — schema and scaling

```yaml
name: linkshortener
description: Read-heavy URL shortener workload

scaling:
  unit: workspace
  max_id_query: "SELECT MAX(id) FROM workspace"
  constants:
    links_per_workspace: 1000

schema:
  tables:
    - name: workspace
      columns:
        - { name: id,   type: "INTEGER NOT NULL" }
        - { name: name, type: "VARCHAR(64)" }
      primary_key: [id]

    - name: link
      columns:
        - { name: id,           type: "BIGINT NOT NULL" }
        - { name: workspace_id, type: "INTEGER NOT NULL" }
        - { name: slug,         type: "VARCHAR(12) NOT NULL" }
        - { name: target,       type: "TEXT NOT NULL" }
        - { name: hits,         type: "BIGINT DEFAULT 0" }
        - { name: created_at,   type: "TIMESTAMP DEFAULT NOW()" }
      primary_key: [id]

  foreign_keys:
    - name: fk_link_workspace
      table: link
      columns: [workspace_id]
      references: { table: workspace, columns: [id] }

  indexes:
    - { name: i_link_ws,   table: link, columns: [workspace_id] }
    - { name: i_link_slug, table: link, columns: [slug], unique: true }
```

### Step 2 — load data (template style)

The slug must be unique, so we derive it from the globally-unique link id
(`(id-1)*1000 + i`), and give each link a random target:

```yaml
load:
  - table: workspace
    copy_columns: [id, name]
    rows_per_id: 1
    row: "{{id}}\tWorkspace-{{id}}\n"

  - table: link
    copy_columns: [id, workspace_id, slug, target, hits]
    rows_per_id: "{{const.links_per_workspace}}"
    batch_size: 5000
    # globally unique id and slug; random hit count
    row: "{{(id - 1) * const.links_per_workspace + i}}\t{{id}}\ts{{(id - 1) * const.links_per_workspace + i}}\thttps://example.com/{{rand_string(8, 16)}}\t{{rand_int(0, 9999)}}\n"
```

### Step 3 — the workload

A read transaction in `simple` mode, and a write in `script` mode (it needs the
next id and a generated slug, which is easier in Lua):

```yaml
transactions:
  - id: 1
    name: redirect
    weight: 80
    mode: simple
    bindings:
      ws:   "rand_int(min_id, max_id)"
      link: "rand_int((ws - 1) * const.links_per_workspace + 1, ws * const.links_per_workspace)"
    sql: |
      UPDATE link SET hits = hits + 1 WHERE id = {{link}} AND workspace_id = {{ws}};

  - id: 2
    name: create-link
    weight: 20
    mode: script
    script_file: lua/create_link.lua
```

> Note the `redirect` binding `link` depends on `ws`. This works because bindings
> evaluate in declaration order (see Limitations), so `ws` is already in scope
> when `link` is computed. Keep each binding to a single expression; for anything
> with control flow, use `script` mode.

`lua/create_link.lua`:

```lua
local ws = rand_int(min_id, max_id)
local slug = rand_string(6, 10)
local target = "https://example.com/" .. rand_string(8, 20)

begin()
-- next id = current max + 1 (simple approach for the example)
local next_id = (query_one("SELECT COALESCE(MAX(id), 0) FROM link") or 0) + 1
exec("INSERT INTO link (id, workspace_id, slug, target) VALUES ($1, $2, $3, $4)",
     next_id, ws, slug, target)
commit()
```

### Step 4 — run it

```bash
pgmeter -d benchmark init --spec linkshortener.yaml -s 500 -j 4
pgmeter -d benchmark run  --spec linkshortener.yaml -c 16 -T 120 -r 15
```

That's a complete, original benchmark — no Rust, no recompile.

---

## 12. Performance tips

- **Load with `COPY`, not `INSERT`.** Template loaders and the `copy_*` API both
  use `COPY`. Avoid `exec("INSERT ...")` in a tight loop for bulk loads.
- **Batch your row generation.** Template loaders already concatenate a whole
  batch into one string per `COPY` round-trip. In `load_script`, build a batch
  with `string.format` and a buffer, then `copy_write` once, rather than one
  `copy_write` per row where practical.
- **Prefer `simple` mode for fixed SQL.** It is a single round-trip with
  precompiled bindings and is the fastest path for pgbench-style transactions.
- **Use `script` mode when logic demands it** — reads feeding writes,
  conditional rollback, loops. The first call on each thread compiles the script
  (not counted in the measured time); later calls are direct function calls.
- **Cache per-thread work in globals.** Since the VM persists, you can memoize a
  formatted query string or a lookup table in a global on first use.
- **`rand_strings` for bulk strings.** When generating many random strings during
  loading, the batched `rand_strings(count, min, max)` reduces Lua↔Rust crossings.

---

## 13. Limitations and gotchas

- **Modes:** only `simple` and `script` are implemented. `extended` (referenced
  in some sample comments) is not — using it errors at run time.
- **`scalefactor` is `0` at run time.** Only loaders see the real value. Derive
  ranges in transactions from `min_id`/`max_id`/`const.*`.
- **`simple`-mode bindings evaluate in declaration order.** Bindings compile into
  a single Lua function in the order written, so a later binding may reference an
  earlier one. Each binding is still a single expression, though — for multi-step
  logic, reads that feed writes, or control flow, use `script` mode.
- **Bindings have no DB access.** `query`/`exec` are only available in `script`
  mode and in loaders, not in `simple`-mode bindings or row templates.
- **Loaders don't get `min_id`/`max_id`.** Iterate the `ids` array instead.
- **Tables are 1-indexed** (Luau): `rows[1]` is the first row, `Q[1]` the first
  query.
- **Shared (non-partitioned) tables** must be loaded from `pre_load_script` or
  guarded by `if id == 1` in `load_script`, otherwise every parallel job will try
  to load them.
- **Parameters are interpolated as literals** (simple protocol), not bound
  server-side. String values are escaped, but treat `$n` as value placeholders
  only — don't build identifiers from untrusted input.

---

## 14. Cheat sheets

### YAML keys

```
name                      benchmark name (required)
description               free text
scaling:
  unit                    label only (not used by engine)
  max_id_query            SQL -> single int (max unit id)
  constants: { k: v }     scalars -> const.k
schema:
  pre_ddls: [sql]         run before CREATE TABLE
  tables:
    - name
      columns: [{name, type}]
      primary_key: [col]
      access_method       USING <m>
      storage: { k: v }   WITH (k=v)
  foreign_keys:
    - { name, table, columns, references: { table, columns } }
  indexes:
    - { name, table, columns, unique, type, include, where }
  post_ddls: [sql]        run after vacuum
load:                     (template loaders; ignored if load_script set)
  - { table, method, copy_columns, rows_per_id, batch_size, row }
pre_load_script           Lua (inline or path), runs once
load_script               Lua (inline or path), runs per job over `ids`
transactions:
  - { id, name, weight, description, mode, bindings, sql, script, script_file }
scripts:
  modules: [path]         Lua preloaded into every VM
```

### Template variables (in `row:` and `rows_per_id:`)

```
id            current unit id
i             row counter 1..rows_per_id   (row: only)
min_id max_id scalefactor const.*
rand_int(min,max)  rand_float(min,max)
```

### Lua functions

```
-- random
rand_int(min, max)                 -> int   [min, max]
rand_float(min, max)               -> num   [min, max)
rand_string(min_len, max_len)      -> string
rand_strings(count, min_len, max_len) -> array

-- transactions
begin()   commit()   rollback()

-- SQL (simple protocol; params -> $1,$2,... as literals)
exec(sql, ...params)               -> nil
query(sql, ...params)              -> array of {colname = value}
query_one(sql, ...params)          -> scalar or nil

-- bulk loading
copy_start(sql)   copy_write(data)   copy_end()

-- abort a transaction (counts as an error)
error("message")
```

### Lua globals

```
transactions: min_id, max_id, const.*        (scalefactor == 0)
loaders:      ids[], scalefactor, const.*
```
