-- Ownership tracking for hosted communities.
--
-- The relay is the authority on ownership (it enforces the compare-and-swap on
-- transfer); this column mirrors it so the desktop client can render the owner
-- and so `not_owner` checks do not need a relay round-trip.
ALTER TABLE communities ADD COLUMN IF NOT EXISTS owner_pubkey TEXT;

-- The relay's own id for the tenant, captured at provisioning time. Transfer
-- addresses the relay by this id, so without it a transfer would need a
-- host->id lookup round-trip against the relay first.
ALTER TABLE communities ADD COLUMN IF NOT EXISTS relay_community_id TEXT;
