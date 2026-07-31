//! Control-plane persistence.
//!
//! Quota reads return `Result<Option<..>>` rather than flattening "no row" into
//! an error, because the two cases have opposite handling: a missing plan is a
//! provisioning bug (deny), while a failed query is an outage (fail soft — see
//! [`crate::plan::QuotaDecision`]).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::plan::{Plan, PlanTier};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("db: {0}")]
    Query(#[from] sqlx::Error),
    #[error("db: stored plan tier {0:?} is not recognised")]
    UnknownTier(String),
}

/// One community as the API renders it. Kept as a named struct (not a tuple)
/// because five of its fields are strings and positional confusion between
/// `slug`, `host`, and `owner_pubkey` would type-check fine and ship broken.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CommunityRow {
    pub id: Uuid,
    pub slug: String,
    pub host: String,
    pub owner_pubkey: Option<String>,
    pub relay_community_id: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
}

/// Loads the billing plan for a community. `Ok(None)` = no plan row.
pub async fn load_plan(pool: &PgPool, community_id: Uuid) -> Result<Option<Plan>, DbError> {
    let row: Option<(String, i32, Option<i32>, i32, bool)> = sqlx::query_as(
        "SELECT tier, seats_purchased, agent_hours_override, agent_hours_used, overage_enabled \
         FROM community_plans WHERE community_id = $1",
    )
    .bind(community_id)
    .fetch_optional(pool)
    .await?;

    let Some((tier, seats, hours_override, hours_used, overage)) = row else {
        return Ok(None);
    };

    let tier = PlanTier::parse(&tier).ok_or_else(|| DbError::UnknownTier(tier.clone()))?;
    Ok(Some(Plan {
        tier,
        seats_purchased: seats as i64,
        agent_hours_override: hours_override.map(|v| v as i64),
        agent_hours_used: hours_used as i64,
        overage_enabled: overage,
    }))
}

/// Number of non-archived communities an account owns.
pub async fn active_community_count(pool: &PgPool, account_id: Uuid) -> Result<i64, DbError> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM communities WHERE account_id = $1 AND archived_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Does any community (archived or not) already claim this host?
///
/// Archived communities keep their host reserved: releasing it would let a
/// stranger squat a workspace address a paying customer may unarchive.
pub async fn host_exists(pool: &PgPool, host: &str) -> Result<bool, DbError> {
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM communities WHERE host = $1)")
            .bind(host)
            .fetch_one(pool)
            .await?;
    Ok(exists)
}

/// The account's bound Nostr pubkey, if any.
pub async fn bound_pubkey(pool: &PgPool, account_id: Uuid) -> Result<Option<String>, DbError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT pubkey FROM nostr_identities WHERE account_id = $1")
            .bind(account_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(p,)| p))
}

/// The account a pubkey is bound to, if any. Used to check a transfer target
/// is a real, sign-in-able owner.
pub async fn account_for_pubkey(pool: &PgPool, pubkey: &str) -> Result<Option<Uuid>, DbError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT account_id FROM nostr_identities WHERE pubkey = $1")
            .bind(pubkey)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Records a provisioned community with its owner and the relay's id for it.
///
/// Idempotent on `host` so a retry after a relay timeout — where the relay may
/// already have created the tenant — refreshes rather than fails.
pub async fn record_community_full(
    pool: &PgPool,
    account_id: Uuid,
    slug: &str,
    host: &str,
    owner_pubkey: &str,
    relay_community_id: Option<&str>,
) -> Result<CommunityRow, DbError> {
    let row = sqlx::query_as::<_, CommunityRow>("INSERT INTO communities (account_id, slug, host, owner_pubkey, relay_community_id) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (host) DO UPDATE SET \
           owner_pubkey = EXCLUDED.owner_pubkey, \
           relay_community_id = COALESCE(EXCLUDED.relay_community_id, communities.relay_community_id) \
         RETURNING id, slug, host, owner_pubkey, relay_community_id, archived_at")
    .bind(account_id)
    .bind(slug)
    .bind(host)
    .bind(owner_pubkey)
    .bind(relay_community_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Seeds the plan row for a newly provisioned community.
pub async fn insert_default_plan(
    pool: &PgPool,
    community_id: Uuid,
    tier: PlanTier,
    seats: i64,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO community_plans (community_id, tier, seats_purchased) \
         VALUES ($1, $2, $3) ON CONFLICT (community_id) DO NOTHING",
    )
    .bind(community_id)
    .bind(tier.as_str())
    .bind(seats as i32)
    .execute(pool)
    .await?;
    Ok(())
}

/// All communities for an account, newest first, archived included so the
/// client can render and unarchive them.
pub async fn list_communities(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<CommunityRow>, DbError> {
    let rows = sqlx::query_as::<_, CommunityRow>(
        "SELECT id, slug, host, owner_pubkey, relay_community_id, archived_at FROM communities \
         WHERE account_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A community row only if this account owns it.
pub async fn community_owned_by(
    pool: &PgPool,
    community_id: Uuid,
    account_id: Uuid,
) -> Result<Option<CommunityRow>, DbError> {
    let row = sqlx::query_as::<_, CommunityRow>("SELECT id, slug, host, owner_pubkey, relay_community_id, archived_at FROM communities WHERE id = $1 AND account_id = $2")
    .bind(community_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Sets or clears the archived marker.
pub async fn set_archived(
    pool: &PgPool,
    community_id: Uuid,
    archived: bool,
) -> Result<CommunityRow, DbError> {
    let row = sqlx::query_as::<_, CommunityRow>(
        "UPDATE communities \
         SET archived_at = CASE WHEN $2 THEN now() ELSE NULL END \
         WHERE id = $1 RETURNING id, slug, host, owner_pubkey, relay_community_id, archived_at",
    )
    .bind(community_id)
    .bind(archived)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Moves a community to a new owning account and pubkey after the relay has
/// accepted the rotation.
pub async fn transfer_owner(
    pool: &PgPool,
    community_id: Uuid,
    new_account_id: Uuid,
    new_owner_pubkey: &str,
) -> Result<CommunityRow, DbError> {
    let row = sqlx::query_as::<_, CommunityRow>(
        "UPDATE communities SET account_id = $2, owner_pubkey = $3 \
         WHERE id = $1 RETURNING id, slug, host, owner_pubkey, relay_community_id, archived_at",
    )
    .bind(community_id)
    .bind(new_account_id)
    .bind(new_owner_pubkey)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// The community id for a relay host, if this control plane provisioned it.
pub async fn community_id_for_host(pool: &PgPool, host: &str) -> Result<Option<Uuid>, DbError> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM communities WHERE host = $1")
        .bind(host)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(id,)| id))
}
