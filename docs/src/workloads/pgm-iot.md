# PGM-IoT

A time-series workload derived from TPCx-IoT: substations with sensors that
stream measurements into one wide table, queried while it grows. Seventy
percent of the transactions are bulk inserts, the rest are the queries a
monitoring system runs: recent aggregates per sensor, a sensor's latest
readings, unacknowledged alerts, threshold changes. The metric is committed
transactions per second; each ingestion transaction inserts a batch of rows.

```bash
pgmeter spec new pgm-iot -o .
pgmeter -d benchmark init --spec pgm-iot -s 20 -j 4
pgmeter -d benchmark run  --spec pgm-iot -c 8 -T 300 -r 30
```

## Scale and data

The scale unit is a **substation**. Per substation:

| Table | Rows per substation | Notes |
|---|---|---|
| `substation` | 1 | |
| `sensor` | 100 | type, unit, min and max thresholds, status |
| `measurement` | 100,000 initially, then grows | 1,000 readings per sensor: timestamp, value, quality; ids from a sequence |
| `alert` | 10 | about 70% of them unacknowledged |

Primary keys, five foreign keys and five indexes: `measurement` on
timestamp, on (sensor, timestamp) and on (substation, timestamp) with the
sensor and value included, a partial index on unacknowledged alerts, and
`sensor` on its substation. The measurement table grows by 100 rows per
ingestion
transaction, so a long run with many clients adds tens of millions of rows;
size the scale factor and the duration together.

## The transactions

| Name | Weight | What it does |
|---|---|---|
| Data-Ingestion | 70 | Inserts a batch of 100 readings for one sensor in a single `INSERT ... SELECT FROM unnest(...)`, timestamps 10 ms apart; 10% of the batches also raise an alert for the sensor |
| Scan-Query | 10 | Per-sensor average, minimum, maximum and count of the last hour's readings of a substation, the ten highest averages |
| Device-Query | 10 | One sensor's row and its 100 latest readings |
| Alert-Query | 5 | The 20 latest unacknowledged alerts of a substation, then acknowledges the oldest of them |
| Threshold-Update | 5 | Sets new minimum and maximum thresholds on one sensor |

The substation and sensor are drawn uniformly. Each transaction is a short
Lua script in `scripts/`, one round trip per statement.

## Deviations from TPCx-IoT

- TPCx-IoT defines a gateway workload run through a YCSB-like driver with
  prescribed record sizes and a fixed ingestion to query ratio measured
  over a warm-up and a measurement interval; PGM-IoT keeps the shape (bulk
  ingestion dominating, time-range and device queries alongside) on a
  relational schema with the queries a PostgreSQL deployment would run.
- The headline figure is committed transactions per second over the mix,
  not the IoTps metric.
- The data grows during the run, so throughput over a long run reflects
  the table's growth as well as the server; keep the duration the same
  between the runs you compare.
