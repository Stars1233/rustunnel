-- System tokens are non-billable; used for synthetic monitoring tunnels
-- driven by the public status page (status.rustunnel.com). PAYG metering
-- and dashboards filter `WHERE system = FALSE` once that aggregation ships.

ALTER TABLE tokens ADD COLUMN IF NOT EXISTS system BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_tokens_system ON tokens (system) WHERE system = TRUE;
