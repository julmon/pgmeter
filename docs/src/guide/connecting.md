# Connecting to PostgreSQL

The connection options are global: they come before the subcommand and apply
to `init`, `run` and `spec` alike. Each one has the environment variable the
PostgreSQL tools use, so an exported `PGHOST`, `PGUSER`, `PGDATABASE` and
`PGPASSWORD` are enough.

| Option | Environment variable | Default | Description |
|---|---|---|---|
| `-H, --host` | `PGHOST` | localhost | Host name, or a socket directory such as `/var/run/postgresql` |
| `-p, --port` | `PGPORT` | 5432 | Port |
| `-U, --username` | `PGUSER` | current OS user | Database user |
| `-d, --dbname` | `PGDATABASE` | current OS user | Database |
| `--sslmode` | `PGSSLMODE` | prefer | TLS requirement, see below |

The password comes from `PGPASSWORD` when it is set, otherwise from the
password file, `PGPASSFILE` or `~/.pgpass`, matched on host, port, database
and user as libpq does. There is no prompt and no `--password` option. The
target shown in the run header has the password redacted.

## TLS

`--sslmode` follows libpq's meaning:

| Value | Behaviour |
|---|---|
| `disable` | Never encrypt |
| `prefer` | Encrypt if the server supports it, fall back to clear text otherwise (the default) |
| `require` | Encrypt, without checking the server's certificate |
| `verify-ca` | Encrypt and verify the server certificate: signed by a CA in the system store and matching the host name |
| `verify-full` | The same; pgmeter does not implement libpq's chain-only `verify-ca`, both modes verify the host name too |

TLS is provided by rustls, so no OpenSSL is needed. The `verify-*` modes use
the operating system's certificate store; a custom CA file (libpq's
`sslrootcert`) and client certificates are not supported yet. A server without
TLS and a `require` or `verify-*` mode fail with one error at connection time.

## Connections pgmeter opens

- `init`: one connection for the schema, the pre-loading and the final
  checkpoint; `-j` connections for the data loading, the primary keys, the
  indexes and the vacuum; one for the foreign keys.
- `run`: one connection to read the id range, then one per client (`-c`) for
  the whole run. Make sure `max_connections` on the server allows a few more
  than the client count.
