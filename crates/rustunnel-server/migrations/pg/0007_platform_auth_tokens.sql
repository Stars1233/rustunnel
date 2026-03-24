-- Email verification and password reset tokens for the Platform API.
--
-- These live in the shared PostgreSQL database (owned by rustunnel-server
-- migrations) so FK constraints to users(id) are enforced at the DB level.
-- The Platform API reads/writes these tables but does not run migrations.

CREATE TABLE IF NOT EXISTS email_verification_tokens (
    token       TEXT PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS password_reset_tokens (
    token       TEXT PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    used_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_evtokens_user ON email_verification_tokens (user_id);
CREATE INDEX IF NOT EXISTS idx_prtokens_user ON password_reset_tokens (user_id);
