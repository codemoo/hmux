//! Pure, bounded codex-lb `/v1/usage` normalization. The caller owns HTTP,
//! freshness and source selection. Contract failures never contain input text.
use crate::{model::format_time, model::parse_time, Error, Provider, Snapshot, Window};
use chrono::{DateTime, TimeZone, Utc};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::fmt;

const MAX_BODY: usize = 1 << 20;
const MAX_LIMITS: usize = 128;

#[derive(Default, Deserialize)]
#[serde(default)]
struct Response {
    #[serde(deserialize_with = "bounded_limits")]
    upstream_limits: Option<Vec<Limit>>,
    account_pool_usage: Option<Pool>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Pool {
    primary: Option<f64>,
    secondary: Option<f64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Limit {
    limit_type: String,
    limit_window: String,
    max_value: f64,
    current_value: f64,
    remaining_value: f64,
    model_filter: Option<String>,
    reset_at: Option<String>,
    source: String,
}

fn bounded_limits<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Limit>>, D::Error> {
    struct Bounded;
    impl<'de> Visitor<'de> for Bounded {
        type Value = Vec<Limit>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an array of at most 128 limits")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while out.len() < MAX_LIMITS {
                match seq.next_element()? {
                    Some(value) => out.push(value),
                    None => return Ok(out),
                }
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("limit count exceeded"));
            }
            Ok(out)
        }
    }
    struct Optional;
    impl<'de> Visitor<'de> for Optional {
        type Value = Option<Vec<Limit>>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an optional limit array")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_seq(Bounded).map(Some)
        }
    }
    deserializer.deserialize_option(Optional)
}

fn reset(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    parse_time(raw).or_else(|| {
        raw.parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .and_then(|v| Utc.timestamp_opt(v, 0).single())
    })
}

fn window(raw: &str) -> &str {
    match raw {
        "5h" | "5hr" | "5hrs" | "5hour" | "5hours" | "primary" => "5h",
        "7d" | "7day" | "7days" | "1w" | "1week" | "1weeks" | "week" | "weekly" | "secondary" => {
            "7d"
        }
        _ => "other",
    }
}

/// Parse an already bounded HTTP body into the public Codex allowlist.
/// `Error::Invalid` means unusable/invalid quota; `Error::Limit` means size or
/// count exceeded. Unknown JSON fields are skipped without a Value tree.
pub fn parse_usage(raw: &[u8], seq: i64, now: DateTime<Utc>) -> Result<Snapshot, Error> {
    if raw.len() > MAX_BODY {
        return Err(Error::Limit);
    }
    if raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::Invalid);
    }
    let response: Response = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    let mut snapshot = Snapshot::degraded(Provider::Codex, seq, now, "ok");
    snapshot.status.stale = false;
    snapshot.status.quota_source = "codex_lb".into();
    let mut seen = false;
    for limit in response.upstream_limits.unwrap_or_default() {
        if !limit.source.eq_ignore_ascii_case("aggregate")
            || !limit.limit_type.eq_ignore_ascii_case("credits")
        {
            continue;
        }
        let _ = limit.model_filter;
        if limit.limit_window.trim().is_empty()
            || !limit.max_value.is_finite()
            || !limit.current_value.is_finite()
            || !limit.remaining_value.is_finite()
            || limit.max_value <= 0.0
            || limit.current_value < 0.0
            || limit.current_value > limit.max_value
            || limit.remaining_value < 0.0
            || limit.remaining_value > limit.max_value
        {
            return Err(Error::Invalid);
        }
        let reset_at = reset(limit.reset_at.as_deref());
        if limit
            .reset_at
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && reset_at.is_none()
        {
            return Err(Error::Invalid);
        }
        let lowered = limit.limit_window.trim().to_ascii_lowercase();
        let normalized = window(&lowered);
        let result = Window {
            used_pct: (limit.current_value / limit.max_value).clamp(0.0, 1.0),
            remaining_seconds: if let Some(t) = reset_at {
                ((t - now).num_milliseconds() as f64 / 1000.0)
                    .round()
                    .max(0.0) as i64
            } else {
                0
            },
            resets_at: reset_at.map(format_time),
        };
        match normalized {
            "5h" => {
                snapshot.rolling_5h = result;
                snapshot.rolling_5h_observed = true;
            }
            "7d" => {
                snapshot.weekly = result;
                snapshot.weekly_observed = true;
            }
            _ => {} // Other quota windows are outside HMux's transport allowlist.
        }
        seen = true;
    }
    if let Some(pool) = response.account_pool_usage {
        // Pool presence changes scope even if both windows are absent.
        snapshot.rolling_5h = Window::default();
        snapshot.weekly = Window::default();
        snapshot.rolling_5h_observed = false;
        snapshot.weekly_observed = false;
        seen = false;
        for (value, is_primary) in [(pool.primary, true), (pool.secondary, false)] {
            if let Some(remaining_pct) = value {
                if !remaining_pct.is_finite() || !(0.0..=100.0).contains(&remaining_pct) {
                    return Err(Error::Invalid);
                }
                let target = if is_primary {
                    &mut snapshot.rolling_5h
                } else {
                    &mut snapshot.weekly
                };
                target.used_pct = (1.0 - remaining_pct / 100.0).clamp(0.0, 1.0);
                if is_primary {
                    snapshot.rolling_5h_observed = true;
                } else {
                    snapshot.weekly_observed = true;
                }
                seen = true;
            }
        }
    }
    if !seen {
        return Err(Error::Invalid);
    }
    snapshot.status.quota_observed_at = Some(format_time(now));
    Ok(snapshot)
}

#[cfg(test)]
#[path = "codex_lb_tests.rs"]
mod tests;
