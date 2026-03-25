# Database Reference

rustunnel-server uses two databases:

- **PostgreSQL** — shared store for API tokens and the tunnel audit log. All server instances in a multi-region fleet point at the same PostgreSQL database.
- **SQLite** — per-instance store for captured HTTP request/response pairs used by the dashboard request inspector. Each server instance manages its own local file.

For single-server self-hosted deployments you may run PostgreSQL on the same host.

---

## Configuration

Both databases are configured in `server.toml`:

```toml
[database]
# PostgreSQL — tokens and tunnel audit log
url = "postgresql://rustunnel:password@db.example.com:5432/rustunnel"

# SQLite — per-instance captured request data
captured_path = "/var/lib/rustunnel/captured.db"
```

The server creates the SQLite file automatically on first startup. The PostgreSQL database must already exist; the server applies migrations automatically at startup.

---

## PostgreSQL schema

### `tokens`

Stores API tokens used to authenticate the CLI client and the REST API.

```sql
CREATE TABLE tokens (
    id           TEXT PRIMARY KEY,        -- UUID v4
    token_hash   TEXT NOT NULL UNIQUE,    -- SHA-256 hex of the raw token
    label        TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,             -- updated on every verify
    scope        TEXT                     -- reserved; NULL = unrestricted
);
```

> **Note**: The raw token value is **never stored**. Only the SHA-256 hash is
> persisted. The raw value is returned once at creation time and cannot be
> recovered from the database.

### `tunnel_log`

Append-only log of every tunnel that has been registered and unregistered.

```sql
CREATE TABLE tunnel_log (
    id               TEXT PRIMARY KEY,
    tunnel_id        TEXT NOT NULL,
    protocol         TEXT NOT NULL,       -- "http" | "tcp"
    label            TEXT NOT NULL,       -- subdomain (HTTP) or port string (TCP)
    session_id       TEXT NOT NULL,
    token_id         TEXT REFERENCES tokens(id) ON DELETE SET NULL,
    registered_at    TIMESTAMPTZ NOT NULL,
    unregistered_at  TIMESTAMPTZ          -- NULL while tunnel is active
);

CREATE INDEX idx_tunnel_log_tunnel_id ON tunnel_log (tunnel_id);
CREATE INDEX idx_tunnel_log_registered ON tunnel_log (registered_at DESC);
```

---

## SQLite schema

### `captured_requests`

HTTP request/response pairs captured by the edge proxy for the request
inspector in the dashboard. Stored locally per server instance.

```sql
CREATE TABLE captured_requests (
    id             TEXT PRIMARY KEY,
    tunnel_id      TEXT NOT NULL,
    conn_id        TEXT NOT NULL,
    method         TEXT NOT NULL,
    path           TEXT NOT NULL,
    status         INTEGER NOT NULL,
    request_bytes  INTEGER NOT NULL DEFAULT 0,
    response_bytes INTEGER NOT NULL DEFAULT 0,
    duration_ms    INTEGER NOT NULL DEFAULT 0,
    captured_at    TEXT NOT NULL,         -- RFC 3339 UTC
    request_body   TEXT,                  -- JSON: {headers, body}; NULL if body exceeded limit
    response_body  TEXT                   -- JSON: {headers, body}; NULL if body exceeded limit
);

CREATE INDEX idx_captured_tunnel ON captured_requests (tunnel_id, captured_at DESC);
```

The `request_body` and `response_body` columns contain JSON of the form:

```json
{
  "headers": { "content-type": ["application/json"] },
  "body": "<raw body string>"
}
```

---

## Common queries

### PostgreSQL — Tokens

Connect with `psql $DATABASE_URL` or any PostgreSQL client.

```sql
-- List all tokens (newest first)
SELECT id, label, scope, created_at, last_used_at
FROM tokens
ORDER BY created_at DESC;

-- Find a token by label
SELECT * FROM tokens WHERE label = 'ci-deploy';

-- Check when a token was last used
SELECT label, last_used_at FROM tokens WHERE id = '<uuid>';

-- Tokens that have never been used
SELECT id, label, created_at
FROM tokens
WHERE last_used_at IS NULL
ORDER BY created_at;

-- Tokens unused for more than 90 days
SELECT id, label, last_used_at
FROM tokens
WHERE last_used_at < NOW() - INTERVAL '90 days'
   OR last_used_at IS NULL;

-- Delete a token by id (revokes access immediately — no grace period)
DELETE FROM tokens WHERE id = '<uuid>';
```

### PostgreSQL — Tunnel history

```sql
-- All tunnels (newest first)
SELECT tunnel_id, protocol, label, registered_at, unregistered_at
FROM tunnel_log
ORDER BY registered_at DESC;

-- Currently active tunnels
SELECT tunnel_id, protocol, label, registered_at
FROM tunnel_log
WHERE unregistered_at IS NULL;

-- Tunnel duration
SELECT registered_at, unregistered_at,
       EXTRACT(EPOCH FROM (COALESCE(unregistered_at, NOW()) - registered_at))::int
         AS duration_seconds
FROM tunnel_log
WHERE tunnel_id = '<tunnel-id>';

-- All tunnels from the last 24 hours
SELECT * FROM tunnel_log
WHERE registered_at > NOW() - INTERVAL '1 day'
ORDER BY registered_at DESC;

-- Count by protocol
SELECT protocol, COUNT(*) AS total FROM tunnel_log GROUP BY protocol;
```

### SQLite — Captured requests

Connect with `sqlite3 /var/lib/rustunnel/captured.db`.

```sql
-- Recent requests for a tunnel
SELECT method, path, status, duration_ms, captured_at
FROM captured_requests
WHERE tunnel_id = '<tunnel-id>'
ORDER BY captured_at DESC
LIMIT 50;

-- Slow requests (over 1 second)
SELECT tunnel_id, method, path, status, duration_ms, captured_at
FROM captured_requests
WHERE duration_ms > 1000
ORDER BY duration_ms DESC;

-- Error responses (5xx)
SELECT tunnel_id, method, path, status, captured_at
FROM captured_requests
WHERE status >= 500
ORDER BY captured_at DESC;

-- Request volume per tunnel
SELECT tunnel_id, COUNT(*) AS requests,
       AVG(duration_ms) AS avg_ms,
       MAX(duration_ms) AS max_ms
FROM captured_requests
GROUP BY tunnel_id
ORDER BY requests DESC;

-- Delete captured requests older than 30 days
DELETE FROM captured_requests
WHERE captured_at < datetime('now', '-30 days');
```

---

## Manually creating a token

The server hashes tokens with SHA-256. You can insert a token directly into
PostgreSQL if you need to bootstrap access without the running server or CLI.

```bash
# 1. Generate a random token value
TOKEN=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "Raw token (save this): $TOKEN"

# 2. Compute the SHA-256 hash
HASH=$(echo -n "$TOKEN" | sha256sum | awk '{print $1}')

# 3. Insert into PostgreSQL
psql "$DATABASE_URL" <<SQL
INSERT INTO tokens (id, token_hash, label, created_at)
VALUES (gen_random_uuid()::text, '$HASH', 'bootstrap', NOW());
SQL
```

Use the raw `$TOKEN` value with the CLI (`--token`) or REST API.

---

## Backup and restore

### PostgreSQL

```bash
# Backup
pg_dump "$DATABASE_URL" > rustunnel-pg-backup.sql

# Restore to a fresh database
psql "$DATABASE_URL" < rustunnel-pg-backup.sql
```

For production, use your PostgreSQL provider's managed backup feature or a
scheduled `pg_dump` via cron.

### SQLite (captured requests)

```bash
# Hot backup (safe while server is running — WAL mode)
sqlite3 /var/lib/rustunnel/captured.db ".backup /tmp/captured-backup.db"

# Restore
systemctl stop rustunnel.service
cp /tmp/captured-backup.db /var/lib/rustunnel/captured.db
systemctl start rustunnel.service
```

---

## Maintenance

### Purging old captured requests

Captured request bodies accumulate on busy tunnels. A weekly cron job to trim
old data:

```bash
# /etc/cron.weekly/rustunnel-trim
sqlite3 /var/lib/rustunnel/captured.db \
  "DELETE FROM captured_requests WHERE captured_at < datetime('now', '-30 days');"
sqlite3 /var/lib/rustunnel/captured.db "VACUUM;"
```

### Database integrity check

```bash
# PostgreSQL
psql "$DATABASE_URL" -c "SELECT COUNT(*) FROM tokens; SELECT COUNT(*) FROM tunnel_log;"

# SQLite
sqlite3 /var/lib/rustunnel/captured.db "PRAGMA integrity_check;"
sqlite3 /var/lib/rustunnel/captured.db "
  SELECT 'captured_requests', COUNT(*) FROM captured_requests;
"
```

---

## Schema migrations

The server applies both PostgreSQL and SQLite migrations automatically on
startup. All `CREATE TABLE` / `CREATE INDEX` statements use `IF NOT EXISTS`,
and additive column changes use `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.

There is no need to run migrations manually. Migration files live in:

```
crates/rustunnel-server/migrations/
├── pg/      ← PostgreSQL (tokens, tunnel_log)
└── local/   ← SQLite (captured_requests)
```
