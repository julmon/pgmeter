# Measuring well

pgmeter reports what the server did during the measured window. Whether two
such numbers can be compared depends on how the runs were set up. These are
the rules that make a comparison hold, learned the hard way.

## Keep everything the same but one thing

The scale factor, the client count, the ramp-up, the duration, the spec, the
server settings, the machine the client runs on: change one of them between
two runs, and the difference you see is that one. The run header printed at
the start of each run and `run.json` in the results directory record the
run parameters, so a results directory can always be matched to its setup.

## Reload before each run

A run changes the data: PGM-B and PGM-C insert history and orders, PGM-IoT
grows its measurement table, autovacuum rearranges pages. Run `init` again
before each measured run so that every run starts from the same tables, the
same statistics and a fresh checkpoint.

## Discard the first run after a load

The first run after `init` is slower than the next ones by a few percent,
sometimes more: the first autovacuum and autoanalyze pass lands in it, and
the buffer cache and the file-system cache are cold. Either do a short
throwaway run after each load, or run the same configuration twice and keep
the second.

## Ramp up, then measure long enough

Use a ramp-up (`-r`) long enough for every client to be connected and busy
and for the caches to settle; 30 to 60 seconds is usual for an OLTP spec
with a few dozen clients. The ramp-up is excluded from every figure. Then
measure for at least a minute or two: short windows are noisy, and
throughput that drifts during the window (a checkpoint, a vacuum, a growing
table) shows in the time-series charts of the report, which are worth a look
before trusting the headline.

## Repeat

Run each configuration at least twice, three times when the difference is
small, and look at the spread before reading anything into it. A difference
smaller than the run-to-run spread is not a result.

## Watch the server, not only the client

The server log tells you when a run was not measuring what you thought:

- **Checkpoints every few seconds** ("checkpoints are occurring too
  frequently"): `max_wal_size` is too small for the write rate. Both runs
  being compared suffer from it unevenly. Raise it, or at least keep it the
  same, and issue a `CHECKPOINT` before each run so none lands in the
  measured window by chance.
- **Lock waits and deadlocks**: a spec of your own with transactions that
  take their locks in different orders will show deadlocks in `error.log`
  and long tails in the latency table. The bundled workloads take theirs in
  a fixed order.
- **Memory pressure and swapping** on a shared machine show up as stalls of
  tens of seconds in the throughput chart, with nothing in the PostgreSQL
  log to explain them.

## Where the client runs

The load generator uses CPU too. When it shares the machine with the
server, every core-second it burns is taken from PostgreSQL, and the
comparison between tools or between pgmeter versions becomes partly a
comparison of load generators. For a server comparison, run pgmeter on
another machine, or at least keep it on the same one for every run. On a
shared machine, simple-mode specs cost less with the default `--threads`
than with one worker per CPU, because the workers compete with the server.

## Compare like with like

A result from a simple-mode spec that sends a transaction as one batch is
not comparable with one that sends the statements one by one (see
[`transactions`](../writing/transactions.md)), a PGM-C result is not
comparable with a PGM-C-client result, and none of the bundled workloads'
figures are comparable with official TPC results. Comparing two servers or
two configurations under the same spec is what the numbers are for.
