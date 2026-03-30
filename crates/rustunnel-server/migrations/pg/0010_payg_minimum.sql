-- Add monthly_minimum_cents to plans for display and billing logic.
-- The actual floor is enforced via Stripe (a flat base-fee price on the subscription).
ALTER TABLE plans ADD COLUMN IF NOT EXISTS monthly_minimum_cents INTEGER NOT NULL DEFAULT 0;

-- PAYG plan: $3.00/month minimum.
UPDATE plans SET monthly_minimum_cents = 300 WHERE name = 'payg';
