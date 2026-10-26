# `transactions`: the workload

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
| `weight` | yes | relative selection weight (the mix is `weight / Σweight`); with `run --once` every transaction with a weight above 0 runs once per client and the values are ignored |
| `description` | no | free text |
| `mode` | no | `simple` (default) or `script` |
| `bindings` | simple | map of variable → Lua expression |
| `sql` | simple | SQL template with `{{var}}` holes: one string (sent in one round trip) or a list of statements (one round trip each) |
| `script` | script | inline Lua source |
| `script_file` | script | path to a `.lua` file (relative to the spec) |

> Only **`simple`** and **`script`** modes exist. (Some sample comments mention
> an `extended` mode: it is not implemented and will error.)

## `simple` mode

Best for fixed SQL with a few random parameters. `sql` takes two shapes:

- **One string**: the whole block is sent to the server in **one round trip**
  (`batch_execute`). You may include several statements separated by `;`; wrap
  them in `BEGIN; ... COMMIT;` if they must be atomic, otherwise each statement
  autocommits.
- **A list of statements**: each element is sent in **its own round trip**, in
  order, inside the same measured transaction. This is how pgbench runs its
  scripts, so use the list form when your numbers must be comparable with
  pgbench's or when the client/server round trips are part of what you measure.
  On a fast network a 7-statement transaction runs about twice as fast in the
  batched form, which is why the two are not comparable.

In both shapes no result rows are returned to the client (this mode is
fire-and-measure); use `script` mode when a later statement depends on an
earlier result.

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
> beyond a one-line derivation (reads that feed writes, branching, loops), use
> `script` mode instead. See [Limitations](limitations.md).

## `script` mode

Best for multi-statement transactions with control flow, reads that feed writes,
conditional rollback, etc. The Lua source (inline `script` or `script_file`)
becomes the transaction body and runs with the full [Lua API](../reference/lua-api.md).

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
