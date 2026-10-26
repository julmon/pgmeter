# Troubleshooting

Errors are printed on stderr as `error: <message>`, with the server's own
message and detail when there is one, and pgmeter exits with status 1.
During a run, per-transaction failures go to `error.log` in the results
directory instead and the run goes on.

## Connecting

**`connection refused`, `no route to host`, `timed out`**: the host or port
is wrong, or the server does not listen on the network. pgmeter reads
`PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE` and `PGPASSWORD` like `psql`; if
`psql` connects with the same settings, pgmeter will.

**`password authentication failed`**: the password comes from `PGPASSWORD`,
or from the password file (`PGPASSFILE`, default `~/.pgpass`) when it is
not set; there is no prompt. A `.pgpass` entry must match the host, port,
database and user exactly or with `*`, and the file must not be readable by
others.

**`server does not support TLS` with `--sslmode require` or `verify-*`**: the
server has no TLS. Use `prefer` or `disable`, or enable TLS on the server.

**A TLS error naming the certificate (`invalid peer certificate`, an
unknown issuer, a host name mismatch) with `verify-ca` or `verify-full`**:
the server's certificate is not signed by a CA in the system store, or does
not match the host name. A custom CA file is not supported yet; use
`require` to encrypt without verifying, or install the CA in the system
store. **`no CA certificate could be loaded from the system store`** means
the store itself is empty or unreadable.

**`too many clients already`**: the server's `max_connections` is below the
client count plus the connections already open. Lower `-c` or raise the
setting.

## Initializing

**``max_id_query returned NULL: no data found, run `pgmeter init` first``**
(or `returned 0`) on `run`: the tables exist but are empty, or `run` was
given a different spec or database than `init`.

**`relation "..." does not exist`** on `run`: `init` was not run against this
database, or the spec's `max_id_query` or transactions name a table the
schema does not create.

**`init` reports a spec problem** such as ``transaction 1 (read): mode `simple`
requires a non-empty `sql` `` or ``transaction 3 (order): mode `script`
requires `script` or `script_file` ``: the spec is invalid and nothing was
touched. The message names the field;
[Writing benchmarks](../writing/index.md) has the rules.

**`deadlock detected` during `init`**: two `init`s are running against the
same tables at the same time, which happens when two benchmarks share table
names in one database. Use one database per benchmark, or run them one at a
time.

**Loading is slow**: raise `-j` up to the server's core count, check that the
loader uses `COPY` (template loaders and the `copy_*` API do) rather than
`INSERT` in a loop, and look at `max_wal_size` and `checkpoint_timeout` on
the server, which bound the write rate of a bulk load.

## Running

**Errors on New-Order with `New-Order rollback: unknown item`**: expected.
PGM-C and PGM-C-client roll back about 1% of their orders as TPC-C
prescribes, and pgmeter counts every rollback as an error. Any other
message in `error.log` is a real failure.

**`current transaction is aborted` in `error.log`**: a statement failed
earlier in the same transaction and a later one ran on the aborted
connection. pgmeter rolls back after a failed transaction before the next
one; inside a script, a statement that fails raises a Lua error that ends
the transaction, so this message means a script caught the error itself and
went on. Look at the first error of that client instead.

**Lua errors with a traceback**: the message names the script and line. The
usual causes are a `$n` placeholder with no argument, a column name that
does not match the query's result (rows are keyed by column name), a
1-indexed table used as if it were 0-indexed, and `scalefactor` used in a
transaction (it is 0 at run time; derive ranges from `min_id`, `max_id` and
`const.*`).

**Throughput collapses partway through a run**: open `report.html` and look
at the throughput chart for the moment it drops, then the server log at
that time. The usual suspects are a checkpoint storm, a vacuum of a large
table, the machine swapping, or a growing table that no longer fits the
cache. [Measuring well](measuring.md) has the checklist.

**The run header says "one thread per client" for a spec you expected to be
fast**: the spec has at least one `script` transaction, which selects the
thread engine. `--threads` only applies when every transaction is in
`simple` mode.

**Ctrl-C does not stop it**: one Ctrl-C ends the run gracefully at the end
of each client's current transaction and writes the results; a second one
aborts immediately with status 130. A client stuck in a long statement
finishes it first.

## Results

**`error_rate` is not `errors / committed`**: it is errors over all
transactions attempted, committed plus failed, in the measured window.

**The CSVs have fewer rows than the duration**: a per-second series has no
row for a second in which the transaction did not run; `pgmeter-stats-*.csv`
has one row per statistic, not per second.

**An interrupted run's throughput looks off**: after a Ctrl-C the figures
are computed over the window that actually ran, not over `-T`; `run.json`
has `"interrupted": true` and `workload_secs` with the real length.
