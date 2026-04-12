-- P2P tunnel support: publisher registry and tunnel_log extensions.

-- Tracks active P2P publisher tunnels so subscribers can discover them by name.
-- Rows are inserted on RegisterTunnel { protocol: P2p } and deleted when the
-- publisher session disconnects (cascaded via remove_session → remove_tunnel).
CREATE TABLE IF NOT EXISTS p2p_tunnels (
    tunnel_id    TEXT PRIMARY KEY,
    tunnel_name  TEXT UNIQUE NOT NULL,
    secret_hash  TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_p2p_tunnels_name ON p2p_tunnels (tunnel_name);

-- Extend tunnel_log with P2P-specific columns.
-- p2p_mode: 'relayed' or 'direct' (Phase 3). NULL for non-P2P tunnels.
-- p2p_peer_session_id: the session_id of the other side of a P2P relay.
ALTER TABLE tunnel_log ADD COLUMN IF NOT EXISTS p2p_mode TEXT;
ALTER TABLE tunnel_log ADD COLUMN IF NOT EXISTS p2p_peer_session_id TEXT;
