# Cheat sheets

## YAML keys

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
  - { id, name, weight, description, mode, bindings, sql (string | [statements]), script, script_file }
scripts:
  modules: [path]         Lua preloaded into every VM
```

## Template variables (in `row:` and `rows_per_id:`)

```
id            current unit id
i             row counter 1..rows_per_id   (row: only)
min_id max_id scalefactor const.*
rand_int(min,max)  rand_float(min,max)
```

## Lua functions

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

## Lua globals

```
transactions: min_id, max_id, const.*        (scalefactor == 0)
loaders:      ids[], scalefactor, const.*
```
