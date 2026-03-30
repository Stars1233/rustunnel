-- Phase 4: Custom subdomain gating per plan.
--
-- allow_custom_subdomains: when true, users on this plan may request a specific
-- subdomain at tunnel registration time. When false, the server always assigns
-- a random subdomain regardless of what the client requests.
--
-- Defaults to false so existing plans (free) are unchanged.
-- Set to true for payg and any future paid plans.

ALTER TABLE plans ADD COLUMN IF NOT EXISTS allow_custom_subdomains BOOLEAN NOT NULL DEFAULT false;

-- Enable custom subdomains for the PAYG plan.
UPDATE plans SET allow_custom_subdomains = true WHERE name = 'payg';
