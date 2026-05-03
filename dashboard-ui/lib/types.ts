export interface Region {
  id: string;
  name: string;
  location: string;
  host: string;
  control_port: number;
  active: boolean;
}

export interface Tunnel {
  tunnel_id: string;
  protocol: string;
  label: string;
  public_url: string;
  connected_since: string;
  request_count: number;
  /** Total bytes proxied through this tunnel (TUNNEL-8 Phase 5). */
  bytes_proxied: number;
  client_addr: string;
  /** Region ID of the server hosting this tunnel (e.g. "eu", "us"). */
  region_id: string;
  /** NAT type reported by the client (P2P tunnels only). */
  nat_type?: string;
  /** Public mapped addresses from STUN probing (P2P tunnels only). */
  mapped_addrs?: string[];
  /** Group identity when this tunnel is part of a load-balancing pool. */
  group?: TunnelGroupRef;
  /** Current health bit. `true` for tunnels without a configured probe. */
  healthy: boolean;
  /** Current consecutive-failure streak from client probes. Resets on healthy. */
  consecutive_failures: number;
  /** Cumulative `TunnelUnhealthy` frames received for this member. */
  total_health_failures: number;
}

/** Lightweight group identity attached to a single Tunnel. */
export interface TunnelGroupRef {
  name: string;
  /** First 8 hex chars of the group's SHA-256 key hash. */
  key_hash_short: string;
  member_count: number;
  healthy_count: number;
}

/** One row from `GET /api/groups`. */
export interface TunnelGroup {
  protocol: string;
  label: string;
  name: string;
  key_hash_short: string;
  region_id: string;
  member_count: number;
  healthy_count: number;
  unhealthy_count: number;
  total_dispatches: number;
  total_health_failures: number;
  members: TunnelGroupMember[];
}

export interface TunnelGroupMember {
  tunnel_id: string;
  session_id: string;
  client_addr: string;
  request_count: number;
  bytes_proxied: number;
  healthy: boolean;
  consecutive_failures: number;
  total_health_failures: number;
  connected_since: string;
  /** Probe type when the member opted into health checks (`tcp` / `http`). */
  health_check_kind?: string;
}

export interface CapturedRequest {
  id: string;
  tunnel_id: string;
  conn_id: string;
  method: string;
  path: string;
  status: number;
  request_bytes: number;
  response_bytes: number;
  duration_ms: number;
  captured_at: string;
  request_body: string | null;
  response_body: string | null;
}

export interface ServerStatus {
  ok: boolean;
  region: { id: string; name: string; location: string };
  active_sessions: number;
  active_tunnels: number;
}

export interface ApiToken {
  id: string;
  label: string;
  token_hash: string;
  created_at: string;
  last_used_at: string | null;
  scope: string | null;
  tunnel_count: number;
}

export interface CreateTokenResponse {
  id: string;
  label: string;
  token: string; // raw value — shown only once
}

export interface TunnelLogEntry {
  id: string;
  tunnel_id: string;
  protocol: string;
  label: string;
  session_id: string;
  token_id: string | null;
  token_label: string | null;
  registered_at: string;
  unregistered_at: string | null;
  /** Region that hosted this tunnel. Null for pre-Phase-3 history rows. */
  region_id: string | null;
}

export interface TunnelHistoryResponse {
  entries: TunnelLogEntry[];
  total: number;
}

export interface ApiClient {
  get: (path: string) => Promise<unknown>;
  del: (path: string) => Promise<number>;
  post: (path: string, body?: unknown) => Promise<unknown>;
}
