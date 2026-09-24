//! Public usage allowlist shared by the Home encoder and gateway receiver.
//! No provider object, credential, producer identity or raw extra crosses here.
use crate::{model::*, Error};
use serde::{de, Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::{collections::BTreeMap, fmt, io};

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct WireSnapshot<'a> {
    schema: i64,
    seq: i64,
    generated_at_utc: String,
    provider: Option<Provider>,
    plan_type: String,
    burn_rate_per_min: f64,
    burn_state: String,
    today_total_tokens: i64,
    today_sessions: i64,
    rolling_5h: Window,
    weekly: Window,
    rolling_5h_observed: bool,
    weekly_observed: bool,
    status: Status,
    #[serde(deserialize_with = "accounts")]
    accounts: Vec<Account>,
    accounts_updated_at: Option<String>,
    #[serde(borrow, deserialize_with = "sources")]
    sources: BTreeMap<String, &'a RawValue>,
}

fn accounts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Account>, D::Error> {
    struct Visitor;
    impl<'de> de::Visitor<'de> for Visitor {
        type Value = Vec<Account>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded accounts")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while out.len() < MAX_ACCOUNTS {
                let Some(row) = a.next_element()? else {
                    return Ok(out);
                };
                out.push(row);
            }
            // Probe only a discarded value; never allocate a 129th account.
            if a.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("too many accounts"));
            }
            Ok(out)
        }
    }
    d.deserialize_any(Visitor)
}

fn sources<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, &'de RawValue>, D::Error> {
    struct Visitor;
    impl<'de> de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, &'de RawValue>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded sources")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(BTreeMap::new())
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(key) = a.next_key::<String>()? {
                if out.len() == 2 || key.len() > 16 || out.contains_key(&key) {
                    return Err(de::Error::custom("invalid sources"));
                }
                out.insert(key, a.next_value()?);
            }
            Ok(out)
        }
    }
    d.deserialize_any(Visitor)
}

fn materialize(w: WireSnapshot<'_>, child: bool) -> Result<Snapshot, Error> {
    // Check before decoding children, so nesting never expands recursively.
    if child && !w.sources.is_empty() {
        return Err(Error::Invalid);
    }
    let mut children = BTreeMap::new();
    for (name, raw) in w.sources {
        let value = serde_json::from_str(raw.get()).map_err(|_| Error::Invalid)?;
        children.insert(name, materialize(value, true)?);
    }
    Ok(Snapshot {
        schema: w.schema,
        seq: w.seq,
        generated_at_utc: w.generated_at_utc,
        provider: w.provider.ok_or(Error::Invalid)?,
        plan_type: w.plan_type,
        burn_rate_per_min: w.burn_rate_per_min,
        burn_state: w.burn_state,
        today_total_tokens: w.today_total_tokens,
        today_sessions: w.today_sessions,
        rolling_5h: w.rolling_5h,
        weekly: w.weekly,
        rolling_5h_observed: w.rolling_5h_observed,
        weekly_observed: w.weekly_observed,
        status: w.status,
        accounts: w.accounts,
        accounts_updated_at: w.accounts_updated_at,
        sources: children,
    })
}

pub fn decode(raw: &[u8]) -> Result<Snapshot, Error> {
    if raw.len() > MAX_SNAPSHOT_BYTES {
        return Err(Error::Limit);
    }
    let wire = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    let snapshot = materialize(wire, false)?;
    validate(&snapshot)?;
    Ok(snapshot)
}

fn timestamp_len(s: &Option<String>) -> bool {
    s.as_ref().is_none_or(|s| s.len() <= 128)
}
fn ratio(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}
fn window(w: &Window) -> bool {
    ratio(w.used_pct) && w.remaining_seconds >= 0 && timestamp_len(&w.resets_at)
}
fn account_window(w: &Option<AccountWindow>) -> bool {
    w.as_ref()
        .is_none_or(|w| ratio(w.used_pct) && timestamp_len(&w.resets_at))
}
fn retry(s: &Snapshot) -> bool {
    let Some(raw) = &s.status.retry_at else {
        return true;
    };
    if raw.len() > 128
        || !(s.status.state == "rateLimited" || (s.status.state == "ok" && s.status.stale))
    {
        return false;
    }
    let Some(reset) = parse_time(raw) else {
        return false;
    };
    let Some(generated) = parse_time(&s.generated_at_utc) else {
        return false;
    };
    let delay = reset - generated;
    delay > chrono::Duration::zero() && delay <= chrono::Duration::hours(24)
}

pub fn validate(s: &Snapshot) -> Result<(), Error> {
    if s.schema != 1
        || s.seq < 0
        || s.generated_at_utc.is_empty()
        || s.generated_at_utc.len() > 128
        || s.burn_state.len() > 32
        || !s.burn_rate_per_min.is_finite()
        || s.burn_rate_per_min < 0.0
        || s.today_total_tokens < 0
        || s.today_sessions < 0
        || s.plan_type != normalize_plan(&s.plan_type)
        || !window(&s.rolling_5h)
        || !window(&s.weekly)
        || s.status.state.is_empty()
        || s.status.state.len() > 64
        || s.status.data_source.len() > 128
        || s.status.quota_source.len() > 128
        || !timestamp_len(&s.status.quota_observed_at)
        || !retry(s)
        || !timestamp_len(&s.accounts_updated_at)
        || s.accounts.len() > MAX_ACCOUNTS
        || s.sources.len() > 2
    {
        return Err(Error::Invalid);
    }
    for (name, child) in &s.sources {
        let allowed = matches!(
            (
                s.provider,
                name.as_str(),
                child.status.quota_source.as_str()
            ),
            (_, "cli", "oauth_api" | "none")
                | (Provider::Claude, "cswap", "claude_swap")
                | (Provider::Codex, "codex-lb", "codex_lb")
        );
        if !allowed || child.provider != s.provider || !child.sources.is_empty() {
            return Err(Error::Invalid);
        }
        validate(child)?;
    }
    let mut numbers = std::collections::BTreeSet::new();
    for a in &s.accounts {
        if a.number <= 0
            || !numbers.insert(a.number)
            || (s.provider == Provider::Codex && !a.email.is_empty())
            || a.email != safe_label(&a.email)
            || a.display_name != safe_label(&a.display_name)
            || a.status.len() > 64
            || !account_window(&a.five_hour)
            || !account_window(&a.seven_day)
            || a.tokens_per_hour.is_some_and(|n| !n.is_finite() || n < 0.0)
            || a.total_tokens.is_some_and(|n| n < 0)
            || !timestamp_len(&a.last_refresh_at)
            || a.plan_type != normalize_plan(&a.plan_type)
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, Error> {
    validate(snapshot)?;
    struct Bounded(Vec<u8>);
    impl io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_SNAPSHOT_BYTES - self.0.len() {
                return Err(io::Error::other("usage limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, snapshot).map_err(|_| Error::Limit)?;
    Ok(out.0)
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
