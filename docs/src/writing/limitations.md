# Limitations and gotchas

- **Modes:** only `simple` and `script` exist. Any other `mode` value is
  rejected when the spec is loaded.
- **TLS:** `--sslmode` uses rustls with the system CA store; a custom CA file
  (libpq's `sslrootcert`) and client certificates are not supported yet.
- **`scalefactor` is `0` at run time.** Only loaders see the real value. Derive
  ranges in transactions from `min_id`/`max_id`/`const.*`.
- **`simple`-mode bindings evaluate in declaration order.** Bindings compile into
  a single Lua function in the order written, so a later binding may reference an
  earlier one. Each binding is still a single expression, though: for multi-step
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
  only: don't build identifiers from untrusted input.
