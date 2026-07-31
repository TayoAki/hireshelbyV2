-- Login codes and sessions for the desktop loopback auth flow.

-- Short-lived, single-use authorization codes.
--
-- The desktop app starts a loopback listener, opens /v1/auth/login in the
-- browser, and the provider redirects back to that listener with a code. The
-- code is then exchanged for a session over HTTPS, so the code itself never
-- needs to be long-lived.
CREATE TABLE login_codes (
    -- SHA-256 of the code. The plaintext is only ever in the redirect URL and
    -- the exchange request body; a database leak must not yield usable codes.
    code_hash    TEXT        PRIMARY KEY,
    account_id   UUID        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The loopback URL this code was minted for. Recorded so an operator can
    -- audit where a code was sent.
    return_to    TEXT        NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    -- Set on first exchange. A second exchange of the same code must fail.
    consumed_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX login_codes_expiry_idx ON login_codes (expires_at)
    WHERE consumed_at IS NULL;

-- Bearer sessions presented as the X-HireShelby-Session header.
CREATE TABLE sessions (
    -- SHA-256 of the credential, never the credential itself.
    credential_hash TEXT        PRIMARY KEY,
    account_id      UUID        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    expires_at      TIMESTAMPTZ NOT NULL,
    revoked_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX sessions_account_idx ON sessions (account_id);
CREATE INDEX sessions_expiry_idx ON sessions (expires_at) WHERE revoked_at IS NULL;

-- Display name for the account, shown in the desktop UI next to the email.
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS name TEXT;
