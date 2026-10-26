# The init lifecycle

`init` performs these steps in order (you can see each in the console
output):

1. **Database DDLs**: `pre_ddls`, then `DROP/CREATE TABLE` for every table
   (with `access_method` / `storage`). Run in one transaction.
2. **Pre-loading**: `pre_load_script`, if present (single connection).
3. **Data loading**: `-j` parallel jobs. The `1..scalefactor` unit ids are
   round-robin distributed across jobs; each job runs the template loaders or
   `load_script` for its ids.
4. **Primary keys**: `ALTER TABLE ... ADD PRIMARY KEY`.
5. **Foreign keys**: unless `--no-fkey`.
6. **Indexes**: all `indexes`.
7. **Vacuum**: `VACUUM FREEZE ANALYZE` per table, then `post_ddls`.
8. **Checkpoint**: forces a checkpoint so the run starts from a clean state.

Keys and indexes are created *after* loading for speed. This ordering is why
your `row` templates / `load_script` only need to produce raw column data: the
constraints are applied afterwards.
