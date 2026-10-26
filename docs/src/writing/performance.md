# Performance tips

- **Load with `COPY`, not `INSERT`.** Template loaders and the `copy_*` API both
  use `COPY`. Avoid `exec("INSERT ...")` in a tight loop for bulk loads.
- **Batch your row generation.** Template loaders already concatenate a whole
  batch into one string per `COPY` round-trip. In `load_script`, build a batch
  with `string.format` and a buffer, then `copy_write` once, rather than one
  `copy_write` per row where practical.
- **Prefer `simple` mode for fixed SQL.** It is a single round-trip with
  precompiled bindings and is the fastest path for pgbench-style transactions.
- **Use `script` mode when logic demands it**: reads feeding writes,
  conditional rollback, loops. The first call on each thread compiles the script
  (not counted in the measured time); later calls are direct function calls.
- **Cache per-thread work in globals.** Since the VM persists, you can memoize a
  formatted query string or a lookup table in a global on first use.
- **`rand_strings` for bulk strings.** When generating many random strings during
  loading, the batched `rand_strings(count, min, max)` reduces Lua↔Rust crossings.
