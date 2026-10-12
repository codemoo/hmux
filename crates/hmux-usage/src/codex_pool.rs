//! API-key-scoped fleet quota. Never join global dashboard exports to this pool.
use crate::{model::*, Error};
use chrono::{DateTime, Duration, Utc};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{collections::BTreeSet, fmt};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fleet {
    #[serde(deserialize_with = "rows")]
    accounts: Vec<Row>,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Row {
    account_id: String,
    // Only an explicit alias is public. Fleet displayName may fall back to email.
    alias: Option<String>,
    display_name: String,
    email: String,
    plan_type: String,
    status: String,
    primary: FleetWindow,
    secondary: FleetWindow,
    usage_refreshed_at: Option<String>,
    capacity_credits_primary: Option<f64>,
    capacity_credits_secondary: Option<f64>,
    remaining_credits_primary: Option<f64>,
    remaining_credits_secondary: Option<f64>,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FleetWindow {
    remaining_percent: Option<f64>,
    reset_at: Option<String>,
    window_minutes: Option<i64>,
}
fn rows<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Row>, D::Error> {
    struct Rows;
    impl<'de> Visitor<'de> for Rows {
        type Value = Vec<Row>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("at most 128 fleet accounts")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while out.len() < MAX_ACCOUNTS {
                let Some(row) = seq.next_element()? else {
                    return Ok(out);
                };
                out.push(row);
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("account limit"));
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Rows)
}
// codex-lb's credit-equivalent capacity table, not an OpenAI guaranteed limit.
// Explicit capacities from the scoped response take priority over this estimate.
fn plan_capacity(plan: &str, weekly: bool) -> Option<f64> {
    let primary = match plan {
        "plus" | "team" | "business" | "edu" => 225.0,
        "pro" | "enterprise" => 1500.0,
        "prolite" => 1125.0,
        "free" if weekly => return Some(1134.0),
        _ => return None,
    };
    Some(if weekly { primary * 33.6 } else { primary })
}
struct Measure {
    used: f64,
    capacity: f64,
    reset: Option<DateTime<Utc>>,
    estimated: bool,
}
fn measure(
    window: &FleetWindow,
    capacity: Option<f64>,
    credits: Option<f64>,
    plan: &str,
    weekly: bool,
    now: DateTime<Utc>,
) -> Option<Measure> {
    if window.window_minutes != Some(if weekly { 10080 } else { 300 }) {
        return None;
    }
    let reset = match window.reset_at.as_deref() {
        Some(raw) => Some(parse_time(raw)?),
        None => None,
    };
    if reset.is_some_and(|at| at <= now) {
        return None;
    }
    let estimated = capacity.is_none();
    let capacity = capacity.or_else(|| plan_capacity(plan, weekly))?;
    if !capacity.is_finite() || capacity <= 0.0 || capacity > 1e12 {
        return None;
    }
    let fraction = match credits {
        Some(remaining) if !estimated => remaining / capacity,
        _ => window.remaining_percent? / 100.0,
    };
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    Some(Measure {
        used: 1.0 - fraction,
        capacity,
        reset,
        estimated,
    })
}
#[derive(Default)]
struct Aggregate {
    used: f64,
    capacity: f64,
    reset: Option<DateTime<Utc>>,
    missing: bool,
    estimated: bool,
}
impl Aggregate {
    fn add(&mut self, m: Option<&Measure>) {
        let Some(m) = m else {
            self.missing = true;
            return;
        };
        self.used += m.used * m.capacity;
        self.capacity += m.capacity;
        self.estimated |= m.estimated;
        if let Some(at) = m.reset {
            self.reset = Some(self.reset.map_or(at, |old| old.min(at)));
        }
    }
    fn window(&self, now: DateTime<Utc>) -> Option<Window> {
        (!self.missing && self.capacity > 0.0).then(|| Window {
            used_pct: (self.used / self.capacity).clamp(0.0, 1.0),
            remaining_seconds: remaining(self.reset, now),
            resets_at: self.reset.map(format_time),
        })
    }
}
pub fn parse_fleet(raw: &[u8], seq: i64, now: DateTime<Utc>) -> Result<Snapshot, Error> {
    if raw.len() > MAX_SNAPSHOT_BYTES {
        return Err(Error::Limit);
    }
    let fleet: Fleet = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    let mut snap = Snapshot::degraded(Provider::Codex, seq, now, "ok");
    snap.status.quota_source = "codex_lb".into();
    let (mut weekly, mut primary) = (Aggregate::default(), Aggregate::default());
    let mut oldest = now;
    let mut seen = BTreeSet::new();
    for (index, row) in fleet.accounts.into_iter().enumerate() {
        if row.account_id.is_empty() || row.account_id.len() > 256 || !seen.insert(row.account_id) {
            return Err(Error::Invalid);
        }
        let plan = normalize_plan(&row.plan_type);
        let eligible = matches!(
            row.status.as_str(),
            "active" | "rate_limited" | "quota_exceeded"
        );
        let excluded = matches!(
            row.status.as_str(),
            "paused" | "disabled" | "deactivated" | "reauth_required"
        );
        let observed = row.usage_refreshed_at.as_deref().and_then(parse_time);
        let fresh = observed
            .is_some_and(|at| now - at < Duration::minutes(30) && at - now <= Duration::minutes(1));
        let week = fresh
            .then(|| {
                measure(
                    &row.secondary,
                    row.capacity_credits_secondary,
                    row.remaining_credits_secondary,
                    &plan,
                    true,
                    now,
                )
            })
            .flatten();
        let short = fresh
            .then(|| {
                measure(
                    &row.primary,
                    row.capacity_credits_primary,
                    row.remaining_credits_primary,
                    &plan,
                    false,
                    now,
                )
            })
            .flatten();
        if eligible {
            weekly.add(week.as_ref());
            primary.add(short.as_ref());
            if let Some(at) = observed {
                oldest = oldest.min(at);
            }
        } else if !excluded {
            weekly.missing = true;
            primary.missing = true;
        }
        let public_window = |m: Option<Measure>| {
            m.map(|m| AccountWindow {
                used_pct: m.used,
                resets_at: m.reset.map(format_time),
            })
        };
        snap.accounts.push(Account {
            number: (index + 1) as i64,
            display_name: row
                .alias
                .as_deref()
                .or_else(|| {
                    (!row.display_name.contains('@')
                        && !row.display_name.eq_ignore_ascii_case(&row.email))
                    .then_some(row.display_name.as_str())
                })
                .map(safe_label)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Account {}", index + 1)),
            active: eligible,
            status: if row.status == "active" {
                "ok".into()
            } else if eligible || excluded {
                row.status
            } else {
                "unavailable".into()
            },
            plan_type: plan,
            quota_observed_at: observed.map(format_time),
            seven_day: public_window(week),
            five_hour: public_window(short),
            ..Default::default()
        });
    }
    if let Some(window) = weekly.window(now) {
        snap.weekly = window;
        snap.weekly_observed = true;
    }
    if let Some(window) = primary.window(now) {
        snap.rolling_5h = window;
        snap.rolling_5h_observed = true;
    }
    snap.status.quota_weighting = if !snap.weekly_observed && !snap.rolling_5h_observed {
        ""
    } else if (snap.weekly_observed && weekly.estimated)
        || (snap.rolling_5h_observed && primary.estimated)
    {
        "plan_estimate"
    } else {
        "reported_capacity"
    }
    .into();
    snap.status.stale = !snap.weekly_observed && !snap.rolling_5h_observed;
    snap.status.quota_observed_at = (!snap.status.stale).then(|| format_time(oldest));
    snap.accounts_updated_at = Some(format_time(now));
    Ok(snap)
}
#[cfg(test)]
#[path = "codex_pool_tests.rs"]
mod tests;
