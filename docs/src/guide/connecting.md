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

The password is read from `PGPASSWORD` only; there is no prompt and no
`--password` option. The target shown in the run header has the password
redacted.

## TLS

`--sslmode` follows libpq's meaning:

| Value | Behaviour |
|---|---|
| `disable` | Never encrypt |
| `prefer` | Encrypt if the server supports it, fall back to clear text otherwise (the default) |
| `require` | Encrypt, without checking the server's certificate |
| `verify-ca` | Encrypt and check that the server certificate is signed by a CA in the system store |
| `verify-full` | As `verify-ca`, and check that the certificate matches the host name |

TLS is provided by rustls, so no OpenSSL is needed. The `verify-*` modes use
the operating system's certificate store; a custom CA file (libpq's
`sslrootcert`) and client certificates are not supported yet. A server without
TLS and a `require` or `verify-*` mode fail with one error at connection time.

## Connections pgmeter opens

- `init`: one connection for the schema and the fixed steps, plus one per
  loader job (`-j`) during data loading.
- `run`: one connection to read the id range, then one per client (`-c`) for
  the whole run. Make sure `max_connections` on the server allows a few more
  than the client count.
