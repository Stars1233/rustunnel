-- Platform Phase 1: users, plans, billing tables + token/tunnel_log extensions.
--
-- These tables are owned by the server migration so FK constraints are enforced.
-- The Platform API (rustunnel-web) populates users/plans/subscriptions; the
-- server only reads user_id from tokens and writes tunnel_log.
--
-- Existing tokens (user_id = NULL) continue to work — limit checks are skipped
-- when user_id IS NULL. See control/session.rs for enforcement logic.

-- ── New tables ─────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS users (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email               TEXT NOT NULL UNIQUE,
    password_hash       TEXT NOT NULL,
    display_name        TEXT,
    stripe_customer_id  TEXT,
    email_verified      BOOLEAN NOT NULL DEFAULT false,
    status              TEXT NOT NULL DEFAULT 'active',  -- active | suspended | banned
    ban_reason          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS plans (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL UNIQUE,  -- free | payg | starter | pro
    stripe_price_id     TEXT,                  -- NULL for free and payg
    billing_model       TEXT NOT NULL,         -- 'free' | 'metered' | 'subscription'
    monthly_price_cents INTEGER NOT NULL DEFAULT 0,
    max_tunnels         INTEGER,               -- NULL = unlimited
    max_connections     INTEGER NOT NULL DEFAULT 100,
    rate_limit_rps      INTEGER NOT NULL DEFAULT 100,
    bandwidth_limit_gb  INTEGER,               -- NULL = unlimited
    history_days        INTEGER NOT NULL DEFAULT 7,
    is_active           BOOLEAN NOT NULL DEFAULT true,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                 UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plan_id                 UUID NOT NULL REFERENCES plans(id),
    stripe_subscription_id  TEXT UNIQUE,    -- NULL for free / payg
    status                  TEXT NOT NULL,  -- active | trialing | past_due | canceled | suspended
    current_period_start    TIMESTAMPTZ,
    current_period_end      TIMESTAMPTZ,
    cancel_at_period_end    BOOLEAN NOT NULL DEFAULT false,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Per-tunnel usage aggregated for billing; written by platform-api billing job.
-- unlimited = true token tunnels are tracked here for visibility but not invoiced.
CREATE TABLE IF NOT EXISTS usage_events (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID NOT NULL REFERENCES users(id),
    tunnel_id         TEXT NOT NULL,
    -- Region that hosted this tunnel. Carried from tunnel_log.region_id.
    region_id         TEXT,
    period_start      TIMESTAMPTZ NOT NULL,
    period_end        TIMESTAMPTZ NOT NULL,
    tunnel_hours      NUMERIC(10, 4) NOT NULL DEFAULT 0,
    request_count     BIGINT NOT NULL DEFAULT 0,
    bytes_proxied     BIGINT NOT NULL DEFAULT 0,
    invoiced          BOOLEAN NOT NULL DEFAULT false,
    stripe_invoice_id TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_usage_events_user_period ON usage_events (user_id, period_start);
CREATE INDEX IF NOT EXISTS idx_usage_events_uninvoiced  ON usage_events (user_id) WHERE NOT invoiced;
CREATE INDEX IF NOT EXISTS idx_usage_events_region      ON usage_events (region_id);

-- ── Extend existing tables ─────────────────────────────────────────────────────

-- tokens: platform billing fields.
-- user_id = NULL → admin/agent token; limit checks skipped for backwards compat.
-- unlimited = true → bypasses expires_at and tunnel_limit; status still enforced.
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS user_id       UUID REFERENCES users(id);
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS expires_at    TIMESTAMPTZ;
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS tier          TEXT;
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS tunnel_limit  INTEGER;  -- NULL = unlimited
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS status        TEXT NOT NULL DEFAULT 'active';
ALTER TABLE tokens ADD COLUMN IF NOT EXISTS unlimited     BOOLEAN NOT NULL DEFAULT false;

CREATE INDEX IF NOT EXISTS idx_tokens_user ON tokens (user_id);

-- tunnel_log: usage tracking for billing aggregation.
-- user_id written from token.user_id at registration time.
-- bytes_proxied and request_count updated at tunnel close.
ALTER TABLE tunnel_log ADD COLUMN IF NOT EXISTS user_id        UUID REFERENCES users(id);
ALTER TABLE tunnel_log ADD COLUMN IF NOT EXISTS bytes_proxied  BIGINT NOT NULL DEFAULT 0;
ALTER TABLE tunnel_log ADD COLUMN IF NOT EXISTS request_count  BIGINT NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_tunnel_log_user ON tunnel_log (user_id, registered_at DESC);

-- ── Seed data ──────────────────────────────────────────────────────────────────

INSERT INTO plans (name, billing_model, max_tunnels, max_connections, rate_limit_rps, bandwidth_limit_gb, history_days)
VALUES
  ('free', 'free',    2,    10,  20,  1,    7),
  ('payg', 'metered', NULL, 100, 100, NULL, 90)
ON CONFLICT (name) DO NOTHING;
