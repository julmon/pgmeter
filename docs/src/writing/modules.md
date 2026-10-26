# `scripts.modules`: shared Lua code

```yaml
scripts:
  modules:
    - lua/helpers.lua
    - lua/queries.lua
```

Each listed file is loaded **once per worker thread**, at global scope, before
any transaction runs, and also into the VM of `pre_load_script` and
`load_script`, so loaders and transactions can share helpers. Use modules for:

- Helper functions shared across transactions.
- Dispatch tables (e.g. `Q[1]..Q[22]` for PGM-H).
- Constant lookup tables (syllable lists, type names, ...).

Anything a module defines as a global is visible to every `script`-mode
transaction on that thread.
