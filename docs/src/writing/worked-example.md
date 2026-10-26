# Worked example: a benchmark from scratch

Let's build a small **link-shortener** benchmark from nothing. Scale unit = a
"workspace"; each workspace owns 1000 links. The workload is 80% redirects
(read a link, bump its hit counter) and 20% link creation.

## Step 1: schema and scaling

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

## Step 2: load data (template style)

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

## Step 3: the workload

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

## Step 4: run it

```bash
pgmeter -d benchmark init --spec linkshortener.yaml -s 500 -j 4
pgmeter -d benchmark run  --spec linkshortener.yaml -c 16 -T 120 -r 15
```

That's a complete, original benchmark: no Rust, no recompile.
