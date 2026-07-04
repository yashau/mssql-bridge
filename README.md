# mssql-bridge

A thin HTTP-to-MSSQL proxy. Speak SQL Server from anywhere that can make a `fetch` call — Cloudflare Workers, serverless functions, edge environments, anything that can't link a TDS driver.

**Design goals**

- One static binary with a small config file.
- No auth layer of its own. Each HTTP request presents SQL Server credentials via HTTP Basic Auth; SQL Server's own RBAC decides what's allowed.
- Memory-bounded under large queries. Streaming endpoint yields rows as TDS delivers them.
- Installs as a Windows service.

---

## Endpoints

### `GET /health`

Returns `ok`. No auth.

### `POST /query`

Buffered query. All rows are collected into a JSON response. Bounded by `limits.max_rows`. Use for normal-sized results.

**Request**

```http
POST /query?rows_as_objects=true
Host: localhost:3001
Authorization: Basic <base64(user:password)>
Content-Type: application/json

{
  "sql": "SELECT TOP 5 Id, Name FROM dbo.Users WHERE Active = @P1",
  "params": [true],
  "database": "MyDb"
}
```

Parameters are positional and bind to `@P1`, `@P2`, ... in order. Supported JSON types: `null`, `bool`, integer, float, string.

`database` is optional when `mssql.default_database` is set in config.

The query string `rows_as_objects=true` emits rows as objects keyed by column name. Default is positional arrays (smaller payload, ordered).

**Response**

```json
{
  "result_sets": [
    {
      "columns": [
        {"name": "Id", "type": "Int4"},
        {"name": "Name", "type": "NVarchar"}
      ],
      "rows": [
        [1, "Alice"],
        [2, "Bob"]
      ]
    }
  ]
}
```

Multiple result sets (from batches / stored procs) produce multiple entries.

### `POST /query/stream`

Same request shape as `/query`. Response is `application/x-ndjson` — one JSON object per line, streamed as rows arrive. Not bounded by `max_rows`. Use for large exports.

**Frames**

```json
{"type":"metadata","result_set":0,"columns":[{"name":"Id","type":"Int4"},...]}
{"type":"row","result_set":0,"values":[1,"Alice"]}
{"type":"row","result_set":0,"values":[2,"Bob"]}
{"type":"end"}
```

On error mid-stream, the last frame is `{"type":"error","message":"..."}`. HTTP status is already 200 by then — callers must check the terminal frame.

Backpressure is honored end-to-end: a slow HTTP reader pauses tiberius, which pauses the TCP read, which pauses SQL Server's producer.

---

## Type mapping

| SQL Server | JSON |
|---|---|
| `bit` | `true` / `false` |
| `tinyint`, `smallint`, `int`, `bigint` | number |
| `real`, `float` | number |
| `decimal`, `numeric`, `money`, `smallmoney` | string (to preserve precision) |
| `datetime`, `datetime2`, `smalldatetime` | string `YYYY-MM-DDTHH:MM:SS[.fff]` |
| `datetimeoffset` | RFC 3339 string |
| `date` | string `YYYY-MM-DD` |
| `time` | string `HH:MM:SS[.fff]` |
| `uniqueidentifier` | string |
| `char`, `varchar`, `nchar`, `nvarchar`, `text`, `ntext`, `xml` | string |
| `binary`, `varbinary`, `image` | base64 string |
| `NULL` | `null` |

---

## Configuration

Copy `config.example.toml` to `config.toml` and edit. Key sections:

- `[server]` bind address, request timeout, body size cap.
- `[mssql]` host, port, `instance_name` for named instances, encryption, application name.
- `[pool]` per-credential connection pool sizing + LRU cap on distinct credential pools.
- `[limits]` `max_rows` (buffered endpoint only), per-query timeout.
- `[log]` level and whether to log SQL text.

Config is resolved in this order: `--config`, `MSSQL_BRIDGE_CONFIG` env var, `./config.toml`, `config.toml` next to the binary.

---

## Development

This project uses [mise](https://mise.jdx.dev/) for tool setup and project tasks.

Available tasks include:

```shell
mise run setup
mise run gate
mise run miri
mise run ci
mise run version:next
mise run version:cut
mise run version:push
mise run release:build
mise run release:package
mise run release:publish
```

Use `mise run setup` to install stable Rust, Clippy, rustfmt, and nightly Miri. Use `mise run gate` for the standard local gate: formatting, Clippy, and tests. Use `mise run ci` for the full gate, including Miri.

## Build

```
mise run release:build
```

The release binary is at `target/release/mssql-bridge` (`.exe` on Windows).

---

## Versioning and releases

Releases use calendar versions: `YYYY-MM-DD-N`, where `N` is the release sequence for that date. Git tags are prefixed with `v`, for example `v2026-07-04-1`.

```shell
mise run version:next                 # print today's next calendar version
mise run version:cut                  # create today's next annotated tag
mise run version:cut 2026-07-04-2     # create a specific annotated tag
mise run version:push 2026-07-04-2    # push a release tag
mise run release:package              # package artifacts from a release tag
mise run release:publish              # publish packaged artifacts to GitHub
```

Push a release tag to publish:

```shell
git push origin v2026-07-04-1
```

Tag pushes run the GitHub release workflow, which validates the tag, runs `mise run ci`, builds release archives for Linux, macOS, and Windows, and uploads them to a GitHub Release.

---

## Run (foreground)

No config file needed for a quick run — all settings have CLI overrides:

```
# Use all defaults: listen on 127.0.0.1:3001, MSSQL on localhost:1433
mssql-bridge

# Override anything inline
mssql-bridge --bind 0.0.0.0:3001 --mssql-host db.local --default-database MyDb

# Named-instance SQL Server
mssql-bridge --mssql-host sqlbox.local --mssql-instance SQLEXPRESS

# Or load config.toml; CLI flags still override any field it defines
mssql-bridge --config ./config.toml --bind 0.0.0.0:8080 --log-sql

# Generate a starter config.toml (writes UTF-8 without BOM; avoids PS redirect issues)
mssql-bridge print-config --output config.toml
```

Full help:

```
mssql-bridge --help
```

CLI flags are grouped by heading (`Server`, `SQL Server`, `Limits`, `Pool`, `Logging`) and every override maps 1:1 to a field in `config.toml`. Environment variable `MSSQL_BRIDGE_CONFIG` sets the config path if `--config` is not given.

---

## Install as a Windows service

Place `mssql-bridge.exe` and `config.toml` in a stable location, for example `C:\Program Files\mssql-bridge\`. From an **elevated** PowerShell:

```powershell
cd 'C:\Program Files\mssql-bridge'
.\mssql-bridge.exe install --config 'C:\Program Files\mssql-bridge\config.toml'
sc.exe start mssql-bridge
```

By default the service runs as `LocalSystem`. For a more restrictive account, use the built-in service account `NT SERVICE\mssql-bridge` or a dedicated local account — configure with `sc.exe config`:

```powershell
sc.exe config mssql-bridge obj= "NT AUTHORITY\LocalService" password= ""
```

Uninstall:

```powershell
.\mssql-bridge.exe uninstall
```

Status / manual control:

```powershell
sc.exe query mssql-bridge
sc.exe stop mssql-bridge
sc.exe start mssql-bridge
```

Logs go to stdout/stderr — Windows Service Manager captures these into the Event Viewer under the `mssql-bridge` source. For structured logging into a file, redirect with a wrapper or set up a logging sink in your process manager of choice.

---

## Named instances

If SQL Server is installed as a named instance (e.g. `HOSTNAME\SQLEXPRESS`), set `mssql.instance_name = "SQLEXPRESS"` in the config and leave `host` as the plain hostname. The bridge uses the SQL Server Browser service on UDP 1434 to discover the actual port. Ensure UDP 1434 is reachable from the bridge host; on the same machine it always is.

---

## Security notes

This service exposes an authenticated SQL execution surface. Treat it like you would SQL Server itself:

- **Never expose it to the public Internet.** Bind to a private IP only, and reach it over Cloudflare Mesh / Tailscale / VPN / Access.
- **Use TLS on the listen side** for anything off-box. This binary is plain HTTP; put it behind a TLS terminator (Cloudflare Tunnel, stunnel, Caddy) if leaving localhost.
- **Use SQL Server permissions, not trust in the caller.** Create DB users per calling service, grant only the tables/procs/operations they need. A compromised HTTP client should compromise only what that SQL login could do.
- **Audit with SQL Server's own logs.** The bridge optionally logs SQL text (`log.log_sql = true`) for diagnostics, but SQL Server's audit facilities are the authoritative record.
- **Body size and row cap.** Adjust `server.max_body_bytes` and `limits.max_rows` for your workload; the defaults are conservative.

---

## Example: calling from a Cloudflare Worker

```js
const body = {
  sql: "SELECT TOP 10 Id, Name FROM dbo.Users WHERE Active = @P1",
  params: [true],
  database: "MyDb",
};

const res = await env.MESH.fetch("http://10.20.30.40:3001/query?rows_as_objects=true", {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "authorization": "Basic " + btoa(`${env.SQL_USER}:${env.SQL_PASSWORD}`),
  },
  body: JSON.stringify(body),
});

if (!res.ok) throw new Error(await res.text());
const { result_sets } = await res.json();
```

For a large export via the streaming endpoint, consume the body as a ReadableStream and split on `\n`.

---

## License

MIT
