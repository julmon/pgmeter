# Lua API

## Execution model

- A spec with at least one `script` transaction runs **one thread per
  client**, because the database bridge below is synchronous. A spec made of
  `simple` transactions only runs on the async engine instead: a pool of
  worker threads (`--threads`) drives one task per client, which costs much
  less CPU per transaction. The run header names the engine in use.
- Each benchmark **client thread** owns one Lua VM, created lazily on its first
  `script`-mode transaction. The VM lives for the whole run.
- On VM creation: context globals are injected, then `scripts.modules` are
  executed in order, then every transaction `script` is compiled into a named
  function. Subsequent executions just call the precompiled function: there is
  no re-parsing per transaction.
- Because the VM persists, **globals set by one transaction survive into the
  next** on the same thread. This is useful for caching (compile a prepared
  string once), but means you should not assume a clean slate each call.
- Data-loading scripts (`pre_load_script` / `load_script`) get a **fresh VM per
  invocation**.

## Globals available to scripts

| Global | In transactions | In loaders |
|--------|-----------------|-----------|
| `min_id`, `max_id` | yes | no |
| `scalefactor` | `0` (unused at run time) | yes |
| `const` (table) | yes | yes |
| `ids` (array) | no | yes (per-job slice) |

Plus the standard Luau library (`string`, `table`, `math`, `tostring`,
`ipairs`, ...). The engine uses **Luau** (via the `mlua` crate), so Luau
semantics apply (notably: **arrays/tables are 1-indexed**).

## Random data functions

| Function | Returns | Notes |
|----------|---------|-------|
| `rand_int(min, max)` | integer | **Inclusive** on both ends: `[min, max]`. |
| `rand_float(min, max)` | number | Half-open: `[min, max)`. |
| `rand_string(min_len, max_len)` | string | Random alphanumeric, length in `[min_len, max_len]`. If `min_len >= max_len`, length is exactly `max_len`. |
| `rand_strings(count, min_len, max_len)` | array | `count` random alphanumeric strings in one call (batched: cheaper than calling `rand_string` in a loop during bulk loads). |

## SQL execution

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

## Transaction control

| Function | SQL issued |
|----------|-----------|
| `begin()` | `BEGIN` |
| `commit()` | `COMMIT` |
| `rollback()` | `ROLLBACK` |

A `script` transaction that should be atomic must call `begin()` at the start
and `commit()` at the end. To abort, call `rollback()`.

## Error handling

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
specification. Because pgmeter counts every Lua error as a transaction error, a
correct PGM-C run shows a New-Order error rate near 1% (about 0.45% of all
transactions), each logged as `New-order rollback: item not found` in
`error.log`; other messages there are real failures. Intentional rollbacks are
not reported separately from errors yet.

## Bulk COPY streaming (loaders)

For high-throughput loading inside `load_script` / `pre_load_script`:

| Function | Description |
|----------|-------------|
| `copy_start(sql)` | Begin a `COPY <table> [(cols)] FROM stdin [options]` stream. You provide the full `COPY` statement, so you can set `NULL AS ''`, choose columns, etc. |
| `copy_write(data)` | Write a chunk of `COPY` text: typically one or more `\t`-separated, `\n`-terminated rows. |
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
