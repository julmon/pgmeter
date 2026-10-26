# `schema`: tables, keys, indexes

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

## `tables`

| Field | Required | Maps to |
|-------|----------|---------|
| `name` | yes | table name |
| `columns` | yes | list of `{ name, type }`. `type` is raw SQL: it can include `NOT NULL`, `DEFAULT ...`, `SERIAL`, etc. |
| `primary_key` | no | `ALTER TABLE ... ADD PRIMARY KEY (...)` (created after loading) |
| `access_method` | no | `CREATE TABLE ... USING <method>` (e.g. `heap`, `columnar`) |
| `storage` | no | `CREATE TABLE ... WITH (k = v, ...)` |

Each table is generated as `DROP TABLE IF EXISTS <name> CASCADE` followed by
`CREATE TABLE`, so re-running `init` is idempotent.

> **Column `type` is passed through verbatim.** Whatever you write lands in the
> `CREATE TABLE` between the column name and the next comma. This is the escape
> hatch for defaults, generated columns, check constraints on a column, etc.

## `foreign_keys`

Emitted as `ALTER TABLE <table> ADD CONSTRAINT <name> FOREIGN KEY (<columns>)
REFERENCES <references.table> (<references.columns>)`, created **after** data is
loaded. Skipped entirely when `init` is run with `--no-fkey`.

## `indexes`

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
| `type` | no | `USING <type>`: `btree` (default), `hash`, `gin`, `gist`, `brin`, ... |
| `include` | no | `INCLUDE (<cols>)`: covering / non-key columns |
| `where` | no | `WHERE <predicate>`: partial index |

The generated statement is:

```
CREATE [UNIQUE] INDEX <name> ON <table> [USING <type>] (<columns>) [INCLUDE (<include>)] [WHERE <where>]
```

> For a benchmarking tool, index shape directly affects the numbers you measure.
> `type`, `include`, and `where` exist so the YAML can reproduce the exact index
> definitions a hand-tuned schema would use.

## `pre_ddls` / `post_ddls`

Free-form SQL statements. `pre_ddls` run before table creation (sequences,
`CREATE EXTENSION`, etc.). `post_ddls` run at the end of the vacuum step (e.g.
`setval` to align a sequence with loaded data).
