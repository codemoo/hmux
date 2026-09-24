//! Pure, bounded normalization of `cswap list --json` command output.
//! The caller owns command execution and cadence. No raw source object leaves this module.
use crate::{model, Account, AccountWindow, Error};
use chrono::{DateTime, Duration, Utc};
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;

pub const MAX_COMMAND_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ACCOUNTS: usize = 128;
const MAX_LAST_GOOD_AGE: Duration = Duration::minutes(30);
const MAX_FUTURE_SKEW: Duration = Duration::minutes(5);

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub accounts: Vec<Account>,
    /// Newest original source measurement, not the command read time.
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct List {
    schema_version: i64,
    active_account_number: Option<i64>,
    #[serde(deserialize_with = "bounded_accounts")]
    accounts: Vec<RawAccount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAccount {
    number: i64,
    email: Option<String>,
    alias: Option<String>,
    active: Option<bool>,
    usage_status: Option<String>,
    usage: Option<RawUsage>,
    usage_fetched_at: Option<String>,
    usage_age_seconds: Option<f64>,
    last_good_usage: Option<RawUsage>,
    last_good_fetched_at: Option<String>,
    last_good_age_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUsage {
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    pct: Option<f64>,
    resets_at: Option<String>,
}

fn bounded_accounts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RawAccount>, D::Error> {
    struct Bounded;
    impl<'de> Visitor<'de> for Bounded {
        type Value = Vec<RawAccount>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an array of at most 128 accounts")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_ACCOUNTS));
            while out.len() < MAX_ACCOUNTS {
                match seq.next_element()? {
                    Some(account) => out.push(account),
                    None => return Ok(out),
                }
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("account limit"));
            }
            Ok(out)
        }
    }
    deserializer.deserialize_seq(Bounded)
}

fn normalize_window(window: Option<RawWindow>) -> Result<Option<AccountWindow>, Error> {
    let Some(window) = window else {
        return Ok(None);
    };
    let pct = window.pct.unwrap_or(0.0);
    if !pct.is_finite() {
        return Err(Error::Invalid);
    }
    let resets_at = window
        .resets_at
        .as_deref()
        .and_then(|raw| model::parse_time(raw.trim()))
        .map(model::format_time);
    Ok(Some(AccountWindow {
        used_pct: (pct / 100.0).clamp(0.0, 1.0),
        resets_at,
    }))
}

fn measurement_time(
    raw: Option<&str>,
    age: Option<f64>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, Error> {
    let aged = match age {
        Some(age) if age.is_finite() && (0.0..=1e9).contains(&age) => {
            Some(now - Duration::nanoseconds((age * 1e9) as i64))
        }
        Some(_) => return Err(Error::Invalid),
        None => None,
    };
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(aged);
    };
    let observed = model::parse_time(raw).ok_or(Error::Invalid)?;
    if observed > now + MAX_FUTURE_SKEW {
        return Err(Error::Invalid);
    }
    let observed = observed.min(now);
    Ok(Some(aged.map_or(observed, |a| a.min(observed))))
}

/// Parse official command output into transport-allowlisted accounts.
/// All parse errors are redacted; no input, email, or command diagnostic is returned.
pub fn parse_command(bytes: &[u8], now: DateTime<Utc>) -> Result<Parsed, Error> {
    if bytes.len() > MAX_COMMAND_BYTES {
        return Err(Error::Limit);
    }
    let list: List = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
    if list.schema_version != 1 {
        return Err(Error::Invalid);
    }
    let mut seen = HashSet::with_capacity(list.accounts.len());
    let mut accounts = Vec::with_capacity(list.accounts.len());
    let mut newest: Option<DateTime<Utc>> = None;
    for raw in list.accounts {
        if raw.number <= 0 || !seen.insert(raw.number) {
            return Err(Error::Invalid);
        }
        let mut account = Account {
            number: raw.number,
            email: {
                let label = model::safe_label(raw.email.as_deref().unwrap_or(""));
                if label.is_empty() {
                    format!("Account {}", raw.number)
                } else {
                    label
                }
            },
            display_name: model::safe_label(raw.alias.as_deref().unwrap_or("")),
            active: raw.active.unwrap_or(false),
            status: {
                let status = raw.usage_status.as_deref().unwrap_or("").trim();
                if status.is_empty() {
                    "unavailable".into()
                } else {
                    status.into()
                }
            },
            ..Account::default()
        };
        let use_last_good = raw.usage.is_none() && raw.last_good_usage.is_some();
        let (usage, fetched_at, age) = if use_last_good {
            (
                raw.last_good_usage,
                raw.last_good_fetched_at,
                raw.last_good_age_seconds,
            )
        } else {
            (raw.usage, raw.usage_fetched_at, raw.usage_age_seconds)
        };
        let has_usage = usage.is_some();
        if let Some(usage) = usage {
            account.five_hour = normalize_window(usage.five_hour)?;
            account.seven_day = normalize_window(usage.seven_day)?;
        }
        let observed = measurement_time(fetched_at.as_deref(), age, now)?;
        if let Some(observed) = observed {
            account.last_refresh_at = Some(model::format_time(observed));
            newest = Some(newest.map_or(observed, |n| n.max(observed)));
            if now - observed > MAX_LAST_GOOD_AGE {
                clear_stale(&mut account);
            }
        } else if has_usage {
            clear_stale(&mut account);
        }
        accounts.push(account);
    }
    let active_count = accounts.iter().filter(|a| a.active).count();
    let active_number = accounts.iter().find(|a| a.active).map_or(0, |a| a.number);
    if active_count > 1
        || list
            .active_account_number
            .is_some_and(|n| n != active_number)
    {
        for account in &mut accounts {
            account.active = false;
        }
    }
    Ok(Parsed {
        accounts,
        updated_at: newest.map(model::format_time),
    })
}

fn clear_stale(account: &mut Account) {
    account.five_hour = None;
    account.seven_day = None;
    if account.status == "ok" {
        account.status = "unavailable".into();
    }
}

/// Last successful command result. Failure handling stays with the caller; a successful
/// zero-account response replaces the cache. Read time never refreshes measurement age.
#[derive(Default)]
pub struct LastGood {
    last: Option<(Parsed, DateTime<Utc>)>,
}
impl LastGood {
    pub fn replace(&mut self, parsed: Parsed, read_at: DateTime<Utc>) {
        self.last = Some((parsed, read_at));
    }
    pub fn current(&self, now: DateTime<Utc>) -> Option<Parsed> {
        let (parsed, read_at) = self.last.as_ref()?;
        if now - *read_at > MAX_LAST_GOOD_AGE {
            return None;
        }
        let mut result = parsed.clone();
        for account in &mut result.accounts {
            let observed = account
                .last_refresh_at
                .as_deref()
                .and_then(model::parse_time);
            if observed.is_none_or(|t| now - t >= MAX_LAST_GOOD_AGE) {
                clear_stale(account);
            }
        }
        Some(result)
    }
}

#[cfg(test)]
#[path = "cswap_tests.rs"]
mod tests;
