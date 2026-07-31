//! Plans, seat limits, and cloud-agent-hour allowances.
//!
//! Per-seat pricing with a **pooled** org-wide agent-hour allowance. Pooled
//! rather than per-seat because agent usage is spiky and concentrates in a few
//! power users; a per-seat cap would throttle exactly the people getting the
//! most value while leaving the rest of the pool unused.
//!
//! ## Fail-soft is deliberate
//!
//! [`QuotaDecision`] separates "this is over the limit" from "we could not
//! determine the limit". A billing-lookup outage must never take a paying
//! customer's workspace offline, so callers allow the action and alert. Only a
//! *known* over-limit state denies.
//!
//! ## Agent-hour accounting is ahead of its caller
//!
//! The seat path is wired to `POST /v1/communities`. The agent-hour path
//! ([`check_agent_hours_available`], [`Plan::agent_hour_allowance`]) has no
//! caller yet because the cloud-agent runtime that would consume it is a later
//! phase. It is implemented and unit-tested now so the billing arithmetic is
//! settled before the runtime is built on top of it — getting the pooled
//! allowance wrong after agents are live would mean re-billing customers.

// Justification for the allow: see "Agent-hour accounting is ahead of its
// caller" above. Remove this attribute when the agent runtime lands.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanTier {
    /// 14-day, no card. Full Team features, small fixed agent-hour pool.
    Trial,
    Team,
    Business,
    /// Negotiated limits; `None` values mean "unlimited" and are resolved from
    /// the database row rather than from these defaults.
    Enterprise,
}

impl PlanTier {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanTier::Trial => "trial",
            PlanTier::Team => "team",
            PlanTier::Business => "business",
            PlanTier::Enterprise => "enterprise",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "trial" => Some(PlanTier::Trial),
            "team" => Some(PlanTier::Team),
            "business" => Some(PlanTier::Business),
            "enterprise" => Some(PlanTier::Enterprise),
            _ => None,
        }
    }

    /// Included pooled agent-hours *per seat* per billing period.
    /// Trial is a flat pool, not per-seat, so it returns 0 here — see
    /// [`Plan::agent_hour_allowance`].
    pub fn agent_hours_per_seat(self) -> i64 {
        match self {
            PlanTier::Trial => 0,
            PlanTier::Team => 15,
            PlanTier::Business => 40,
            PlanTier::Enterprise => 0,
        }
    }
}

/// Flat agent-hour pool granted to a trial, independent of seat count.
pub const TRIAL_AGENT_HOURS: i64 = 10;

#[derive(Debug, Clone)]
pub struct Plan {
    pub tier: PlanTier,
    pub seats_purchased: i64,
    /// Overrides the tier default when set (enterprise contracts).
    pub agent_hours_override: Option<i64>,
    /// Agent-hours consumed in the current billing period.
    pub agent_hours_used: i64,
    /// Whether the customer opted in to metered overage beyond the allowance.
    pub overage_enabled: bool,
}

impl Plan {
    /// Total pooled agent-hours available this period.
    pub fn agent_hour_allowance(&self) -> i64 {
        if let Some(explicit) = self.agent_hours_override {
            return explicit;
        }
        match self.tier {
            PlanTier::Trial => TRIAL_AGENT_HOURS,
            PlanTier::Enterprise => 0, // must be set explicitly via the override
            tier => tier
                .agent_hours_per_seat()
                .saturating_mul(self.seats_purchased),
        }
    }

    pub fn agent_hours_remaining(&self) -> i64 {
        (self.agent_hour_allowance() - self.agent_hours_used).max(0)
    }
}

/// Outcome of a quota check.
///
/// `Undetermined` exists so a lookup failure is never silently treated as
/// either allow or deny at the call site — the caller must handle it, and the
/// documented handling is allow + alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaDecision {
    Allow,
    Deny { reason: String },
    Undetermined { reason: String },
}

impl QuotaDecision {
    /// Fail-soft resolution: anything but a definitive `Deny` permits the action.
    pub fn permits(&self) -> bool {
        !matches!(self, QuotaDecision::Deny { .. })
    }
}

/// Can this org add one more seat?
pub fn check_seat_available(plan: &Plan, seats_in_use: i64) -> QuotaDecision {
    if seats_in_use < plan.seats_purchased {
        return QuotaDecision::Allow;
    }
    QuotaDecision::Deny {
        reason: format!(
            "seat limit reached: {} of {} seats in use on the {} plan. Add seats to invite more members.",
            seats_in_use,
            plan.seats_purchased,
            plan.tier.as_str()
        ),
    }
}

/// Can this org start another cloud agent?
///
/// Denies only when the pool is exhausted *and* overage was not enabled, so a
/// customer who opted in to metered overage is never interrupted mid-work.
pub fn check_agent_hours_available(plan: &Plan) -> QuotaDecision {
    if plan.agent_hours_remaining() > 0 || plan.overage_enabled {
        return QuotaDecision::Allow;
    }
    QuotaDecision::Deny {
        reason: format!(
            "cloud agent hours exhausted: {} of {} used this period. Enable overage or upgrade to continue.",
            plan.agent_hours_used,
            plan.agent_hour_allowance()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(tier: PlanTier, seats: i64) -> Plan {
        Plan {
            tier,
            seats_purchased: seats,
            agent_hours_override: None,
            agent_hours_used: 0,
            overage_enabled: false,
        }
    }

    #[test]
    fn team_allowance_is_pooled_across_seats() {
        // 5 seats x 15h = 75h shared, not 15h each.
        assert_eq!(plan(PlanTier::Team, 5).agent_hour_allowance(), 75);
    }

    #[test]
    fn business_pools_at_a_higher_rate() {
        assert_eq!(plan(PlanTier::Business, 5).agent_hour_allowance(), 200);
    }

    #[test]
    fn trial_pool_is_flat_and_ignores_seat_count() {
        assert_eq!(
            plan(PlanTier::Trial, 1).agent_hour_allowance(),
            TRIAL_AGENT_HOURS
        );
        assert_eq!(
            plan(PlanTier::Trial, 50).agent_hour_allowance(),
            TRIAL_AGENT_HOURS
        );
    }

    #[test]
    fn enterprise_requires_an_explicit_override() {
        // Without a contract value the default is 0, which denies rather than
        // silently granting an unbounded pool.
        assert_eq!(plan(PlanTier::Enterprise, 100).agent_hour_allowance(), 0);
        let mut p = plan(PlanTier::Enterprise, 100);
        p.agent_hours_override = Some(5_000);
        assert_eq!(p.agent_hour_allowance(), 5_000);
    }

    #[test]
    fn seat_check_denies_only_at_the_limit() {
        let p = plan(PlanTier::Team, 3);
        assert_eq!(check_seat_available(&p, 2), QuotaDecision::Allow);
        assert!(matches!(
            check_seat_available(&p, 3),
            QuotaDecision::Deny { .. }
        ));
    }

    #[test]
    fn downgrade_blocks_the_next_seat_add() {
        // The scenario that makes the paid tier real: 5 seats in use, plan
        // drops to 3, the next invite must fail.
        let downgraded = plan(PlanTier::Team, 3);
        assert!(matches!(
            check_seat_available(&downgraded, 5),
            QuotaDecision::Deny { .. }
        ));
    }

    #[test]
    fn agent_hours_deny_when_pool_is_spent() {
        let mut p = plan(PlanTier::Team, 1); // 15h
        p.agent_hours_used = 15;
        assert!(matches!(
            check_agent_hours_available(&p),
            QuotaDecision::Deny { .. }
        ));
    }

    #[test]
    fn overage_opt_in_keeps_agents_running_past_the_pool() {
        let mut p = plan(PlanTier::Team, 1);
        p.agent_hours_used = 999;
        p.overage_enabled = true;
        assert_eq!(check_agent_hours_available(&p), QuotaDecision::Allow);
    }

    #[test]
    fn remaining_never_goes_negative() {
        let mut p = plan(PlanTier::Team, 1);
        p.agent_hours_used = 100;
        assert_eq!(p.agent_hours_remaining(), 0);
    }

    #[test]
    fn undetermined_is_fail_soft_but_distinct_from_allow() {
        let undetermined = QuotaDecision::Undetermined {
            reason: "billing lookup timed out".into(),
        };
        assert!(
            undetermined.permits(),
            "a billing outage must not lock out a paying customer"
        );
        assert_ne!(undetermined, QuotaDecision::Allow);
    }

    #[test]
    fn tier_round_trips_through_its_string_form() {
        for tier in [
            PlanTier::Trial,
            PlanTier::Team,
            PlanTier::Business,
            PlanTier::Enterprise,
        ] {
            assert_eq!(PlanTier::parse(tier.as_str()), Some(tier));
        }
        assert_eq!(PlanTier::parse("free"), None);
    }
}
