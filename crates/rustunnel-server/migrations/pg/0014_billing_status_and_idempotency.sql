-- 0014: billing correctness — status vocabulary, churn history, webhook idempotency.
--
-- Three related fixes:
--   1. Normalise subscription status spelling to the canonical 'canceled'.
--   2. Record when a subscription was canceled, so churned-but-previously-paid
--      users can be identified (win-back campaigns) after their effective plan
--      has already reverted to free.
--   3. Give Stripe webhooks an idempotency key so redelivered events cannot be
--      applied twice.

-- ── 1. Status vocabulary ─────────────────────────────────────────────────────
-- platform-api's webhook handler wrote 'cancelled' (en-GB) while every reader
-- compares against 'canceled' (en-US, the spelling documented in 0006). Because
-- 'cancelled' != 'canceled' is TRUE, every `status != 'canceled'` filter was a
-- silent no-op and canceled subscriptions were treated as live everywhere.
-- The writer is fixed in platform-api; this normalises rows already stored.
UPDATE subscriptions SET status = 'canceled' WHERE status = 'cancelled';

-- Stop the two spellings ever diverging again.
ALTER TABLE subscriptions DROP CONSTRAINT IF EXISTS subscriptions_status_check;
ALTER TABLE subscriptions ADD CONSTRAINT subscriptions_status_check
    CHECK (status IN ('active', 'trialing', 'past_due', 'canceled', 'suspended'));

-- ── 2. Churn history ─────────────────────────────────────────────────────────
-- No column recorded *when* a subscription ended, and the cancellation webhook
-- did not even touch updated_at, so there was nothing to drive a "was paid,
-- churned on <date>" view.
ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS canceled_at TIMESTAMPTZ;

-- Backfill what we can for rows already canceled: current_period_end is the end
-- of the last paid period, the closest available proxy for the churn date.
UPDATE subscriptions
   SET canceled_at = current_period_end
 WHERE status = 'canceled'
   AND canceled_at IS NULL
   AND current_period_end IS NOT NULL;

-- ── 3. Webhook idempotency ───────────────────────────────────────────────────
-- Stripe redelivers events on timeout or non-2xx. Nothing recorded event.id, so
-- a redelivered invoice.created would add a SECOND overage invoice item and
-- overbill the customer. The primary key makes replay detection a plain insert.
CREATE TABLE IF NOT EXISTS stripe_events (
    id            TEXT PRIMARY KEY,          -- Stripe event id (evt_...)
    type          TEXT NOT NULL,
    processed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Retention sweeps delete old rows by age.
CREATE INDEX IF NOT EXISTS idx_stripe_events_processed_at
    ON stripe_events (processed_at);

-- ── Supporting indexes ───────────────────────────────────────────────────────
-- subscriptions had no indexes at all; every read does user_id lookups and the
-- admin/reconciliation paths filter on status.
CREATE INDEX IF NOT EXISTS idx_subscriptions_user_id ON subscriptions (user_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_status  ON subscriptions (status);
