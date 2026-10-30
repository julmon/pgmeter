# Data loading

There are two mutually exclusive styles. Pick the one that fits:

| Style | Use when | Key |
|-------|----------|-----|
| **Template** | Rows are a simple function of `id` and a row counter `i`. | `load:` |
| **Lua script** | Loading needs control flow, lookups, cross-table coordination, or shared reference tables. | `load_script:` (+ optional `pre_load_script:`) |

If `load_script` is present it takes precedence and `load:` is ignored.

## Template-based loading (`load:`)

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
| `batch_size` | no | rows per `COPY` round-trip (default 10000, must be above 0) |
| `row` | yes | the **row template** (see below) |

### The row-template mini-language

A `row` string is literal text with `{{ lua_expression }}` holes. The engine
compiles it once into a Lua function and calls it in batches.

- Output must be **tab-separated** (`\t`) values in `copy_columns` order, ending
  with a newline (`\n`): this is PostgreSQL's default `COPY` text format.
- `{{expr}}` is replaced by `tostring(expr)`.
- Two loop variables are available:
  - `id`: the current unit id.
  - `i`: the row counter, running `1..rows_per_id`.
- `rand_int`, `rand_float`, `min_id`, `max_id`, `scalefactor`, and `const.*` are
  all in scope. Database functions (`query`, `exec`, ...) are **not**: template
  loading is pure generation.

Examples:

```yaml
# one row per unit: "<id>\t0\n"
row: "{{id}}\t0\n"

# N rows per unit with a globally unique id, random price and stock
row: "{{(id - 1) * 100 + i}}\t{{id}}\tProduct-{{i}}\t{{rand_float(1.0, 999.99)}}\t{{rand_int(0, 10000)}}\n"
```

### NULLs and empty values in templates

The default `COPY` text format treats an empty field as an empty string and the
two-character sequence `\N` as SQL `NULL`. `CHAR(n)` columns left empty (as in
the PGM-B `filler` columns) are stored blank-padded. If you need a real `NULL`,
emit `\N`, or switch to a Lua loader where you control the `COPY` options.

## Lua-based loading (`load_script` / `pre_load_script`)

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

- **`load_script`** runs once per unit id, on the job that unit is assigned
  to (the engine round-robins the `1..scalefactor` ids across `-j` jobs). The
  global `ids` is an array holding that unit id, so a script loops over `ids`
  and loads each unit's data, typically via the `copy_*` streaming API:

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
and COPY helpers). `min_id`/`max_id` are **not** injected into loaders: iterate
`ids` instead.

> **Parallelism caveat:** because each call sees one unit and the jobs run
> concurrently, a `load_script` must compute any global offsets from the `id`
> value itself (as above with `(id - 1) * N + i`), never from a running
> counter. If a table is *not* partitioned by unit (a single shared table),
> load it from `pre_load_script`, or guard it with `if id == 1 then ... end`
> inside `load_script` so only one job creates it. Loaders that draw every
> value with the `rand_*` functions are reproducible with `init --seed`, which
> reseeds the generator per unit.
