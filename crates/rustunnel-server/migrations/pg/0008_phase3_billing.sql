-- Phase 3: PAYG billing — spend cap support.
--
-- spend_cap_cents: user-defined monthly spending limit.
-- NULL = no cap (default — no limit enforced).
-- When the platform-api daily job estimates the user's MTD bill >= this value,
-- their tokens are suspended until the next billing period.

ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS spend_cap_cents INTEGER;
