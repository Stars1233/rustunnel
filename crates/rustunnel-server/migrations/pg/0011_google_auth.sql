-- Add auth_method column to track how the user signed up (password vs google).
-- All existing users default to 'password'.
ALTER TABLE users ADD COLUMN IF NOT EXISTS auth_method TEXT NOT NULL DEFAULT 'password';

-- Google subject ID for future-proofing (unique per Google account).
ALTER TABLE users ADD COLUMN IF NOT EXISTS google_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_google_id ON users (google_id) WHERE google_id IS NOT NULL;
