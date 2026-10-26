# PGM-C-client

The [PGM-C](pgm-c.md) order-entry workload, driven from the client instead
of from PL/pgSQL functions: each transaction is a Lua script that sends every
statement in its own round trip, reads the results it needs and decides what
to do next, the way an application without stored procedures behaves.

```bash
pgmeter spec new pgm-c-client -o .
pgmeter -d benchmark init --spec pgm-c-client -s 10 -j 4
pgmeter -d benchmark run  --spec pgm-c-client -c 8 -T 120 -r 60
```

The schema, the data, the transaction mix, the input distributions and the
designed 1% New-Order rollback are the same as PGM-C's; the two specs share
their loader and their Lua helpers. What differs is where the transaction
logic runs:

| | PGM-C | PGM-C-client |
|---|---|---|
| Transaction logic | five PL/pgSQL functions created at `init` | five Lua scripts in `scripts/` |
| Round trips per New-Order | 1 | about 45 (one per statement, 5 to 15 lines) |
| Round trips per Payment | 1 | 7 to 8 |
| Execution engine | one thread per client | one thread per client |
| What dominates | server CPU | client-server latency |

On the same server PGM-C-client reports a much lower rate than PGM-C, and
the ratio between the two is a direct measure of what the network and the
protocol cost your application. Use PGM-C-client when that is what you want
to see, or when the thing under test is a connection pooler, a proxy or a
network path. Use PGM-C to measure the server itself.

The error you should expect is the same: about 1% of New-Orders end with
`New-Order rollback: unknown item`, which is the specification's rollback,
counted as an error. The deviations from TPC-C listed on the PGM-C page apply
here too.
