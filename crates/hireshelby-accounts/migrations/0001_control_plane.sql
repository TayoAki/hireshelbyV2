-- HireShelby control-plane schema.
--
-- This database is deliberately separate from the relay's. The relay stores
-- tenant *content*; this stores who is allowed to have a tenant and what they
-- have paid for. Keeping them apart means a control-plane outage degrades
-- signup and billing, not live workspaces.

CREATE TABLE accounts (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email         TEXT        NOT NULL,
    -- Identifier from the external identity provider (WorkOS). We never store
    -- passwords; authentication is delegated entirely.
    external_id   TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (external_id)
);

-- Case-insensitive uniqueness: Alice@x.com and alice@x.com are one account.
CREATE UNIQUE INDEX accounts_email_lower_idx ON accounts (lower(email));

-- Nostr identities bound to an account, proved by a signed challenge.
--
-- An account may bind several pubkeys (desktop, mobile, a second machine), but
-- a pubkey belongs to at most one account — otherwise two accounts could claim
-- the same workspace identity.
CREATE TABLE nostr_identities (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id  UUID        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    pubkey      TEXT        NOT NULL,
    bound_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pubkey)
);

CREATE INDEX nostr_identities_account_idx ON nostr_identities (account_id);

-- Short-lived binding challenges. Rows are consumed on verify and swept after
-- expiry; a challenge must never be reusable.
CREATE TABLE identity_challenges (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id  UUID        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    nonce       TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (nonce)
);

CREATE INDEX identity_challenges_expiry_idx ON identity_challenges (expires_at)
    WHERE consumed_at IS NULL;

-- Communities this control plane has provisioned on the relay.
--
-- `host` mirrors the relay's own key for a tenant, so this table can be
-- reconciled against the relay without a join table.
CREATE TABLE communities (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id   UUID        NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    slug         TEXT        NOT NULL,
    host         TEXT        NOT NULL,
    archived_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (host)
);

CREATE INDEX communities_account_idx ON communities (account_id);

-- Billing state, one row per community.
--
-- Seat and agent-hour limits live here rather than on the account so a single
-- customer can run several communities on different plans.
CREATE TABLE community_plans (
    community_id         UUID PRIMARY KEY REFERENCES communities (id) ON DELETE CASCADE,
    tier                 TEXT        NOT NULL CHECK (tier IN ('trial', 'team', 'business', 'enterprise')),
    seats_purchased      INTEGER     NOT NULL DEFAULT 1 CHECK (seats_purchased >= 0),
    -- NULL means "use the tier default"; set only for negotiated contracts.
    agent_hours_override INTEGER     CHECK (agent_hours_override IS NULL OR agent_hours_override >= 0),
    agent_hours_used     INTEGER     NOT NULL DEFAULT 0 CHECK (agent_hours_used >= 0),
    overage_enabled      BOOLEAN     NOT NULL DEFAULT FALSE,
    -- Stripe linkage. Nullable so a trial exists before any checkout.
    stripe_customer_id     TEXT,
    stripe_subscription_id TEXT,
    trial_ends_at        TIMESTAMPTZ,
    period_started_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Stripe webhooks retry and can deliver out of order, so handlers must be
-- idempotent. Recording every processed event id makes replays a no-op.
CREATE TABLE processed_stripe_events (
    event_id     TEXT PRIMARY KEY,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Metered cloud-agent runtime, billed on wall-clock seconds rather than turn
-- count because turn length varies by orders of magnitude.
CREATE TABLE agent_runtime_sessions (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    community_id  UUID        NOT NULL REFERENCES communities (id) ON DELETE CASCADE,
    agent_pubkey  TEXT        NOT NULL,
    started_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- NULL while the sandbox is running. A row that stays open is a billing
    -- leak, so the reaper alerts on these.
    ended_at      TIMESTAMPTZ,
    billed_seconds INTEGER    CHECK (billed_seconds IS NULL OR billed_seconds >= 0)
);

CREATE INDEX agent_runtime_open_idx ON agent_runtime_sessions (community_id)
    WHERE ended_at IS NULL;
