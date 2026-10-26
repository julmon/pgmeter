# `scaling`: scale factor and context

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
| `unit` | no | A human label for the scale-factor entity (e.g. `branch`, `warehouse`). **Descriptive only**: it is not used by the engine. |
| `max_id_query` | yes | SQL returning a single integer: the largest scale-unit id present. Run once at the start of each `run` to set `max_id`. Must return an `int`. |
| `constants` | no | Scalar values (number / string / bool) exposed to expressions and scripts as `const.<name>`. |

## The scale-factor model

pgmeter loads `-s N` **units**, numbered `1..N`. The unit id is the partitioning
key for everything:

- During loading, each unit id (`id`) is processed once, and you generate the
  rows that belong to it.
- During a run, every transaction picks a random unit id in `[min_id, max_id]`
  and operates on that unit's data.

This is what makes data generation and the workload scale together: at scale
factor 1000 you get 1000 branches, and the workload spreads across branches
`1..1000`.

## Context variables

The following are injected into the Lua environment used for bindings, row
templates, and scripts:

| Variable | Available in | Value |
|----------|-------------|-------|
| `min_id` | bindings, templates, scripts | always `1` |
| `max_id` | bindings, templates, scripts | auto-detected at the start of each run via `max_id_query` |
| `scalefactor` | **loading only** | from `-s` at init time. **`0` during a run**: do not rely on it in transactions. |
| `const.<name>` | everywhere | values from `scaling.constants` |
| `ids` | load scripts only | array of unit ids assigned to the current job |
| `i` | row templates only | row counter, `1..rows_per_id` |
| `id` | row templates only | current unit id |

> ⚠️ **`scalefactor` is `0` while a benchmark runs.** It only carries the real
> value during `init`/loading. In transaction logic, derive ranges from
> `min_id`, `max_id`, and `const.*` instead.

`constants` must be scalars (numbers, strings, booleans). Nested lists/maps are
ignored when injected into Lua.
