//! Control-plane persistence.
//!
//! Quota reads return `Result<Option<..>>` rather than flattening "no row" into
//! an error, because the two cases have opposite handling: a missing plan is a
//! provisioning bug (deny), while a failed query is an outage (fail soft — see
//! [`crate::plan::QuotaDecision`]).

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

/// Loads the billing plan for a community.
///
/// `Ok(None)` means the community has no plan row at all.
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

/// Number of communities an account currently owns, excluding archived ones.
pub async fn active_community_count(pool: &PgPool, account_id: Uuid) -> Result<i64, DbError> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM communities WHERE account_id = $1 AND archived_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Records a community this control plane provisioned on the relay.
///
/// Idempotent on `host` so a retry after a relay timeout — where the relay may
/// already have created the tenant — does not fail or duplicate the row.
pub async fn record_community(
    pool: &PgPool,
    account_id: Uuid,
    slug: &str,
    host: &str,
) -> Result<Uuid, DbError> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO communities (account_id, slug, host) VALUES ($1, $2, $3) \
         ON CONFLICT (host) DO UPDATE SET host = EXCLUDED.host \
         RETURNING id",
    )
    .bind(account_id)
    .bind(slug)
    .bind(host)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Seeds the plan row for a newly provisioned community.
///
/// Trials start here; a Stripe webhook later upgrades the tier in place.
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

/// All communities for an account, newest first, including archived ones so the
/// client can show and unarchive them.
pub async fn list_communities(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<(Uuid, String, String, bool)>, DbError> {
    let rows: Vec<(Uuid, String, String, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT id, slug, host, archived_at FROM communities \
         WHERE account_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, slug, host, archived_at)| (id, slug, host, archived_at.is_some()))
        .collect())
}
