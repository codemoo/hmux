//! Pure parser for codex-lb's derived account export. The caller supplies the
//! file mtime and owns safe file reads, caching and last-good retention.
use crate::{
    model::{format_time, normalize_plan, parse_time, safe_label},
    Account, AccountWindow, Error,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{collections::BTreeSet, fmt};

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 128;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Export {
    schema_version: i64,
    accounts_updated_at: Option<String>,
    #[serde(deserialize_with = "bounded_accounts")]
    accounts: Option<Vec<Row>>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Row {
    number: i64,
    #[serde(rename = "accountId")]
    account_id: Option<String>,
    email: Option<String>,
    alias: Option<String>,
    display_name: Option<String>,
    status: Option<String>,
    five_hour_pct: Option<f64>,
    seven_day_pct: Option<f64>,
    reset_at_primary: Option<String>,
    reset_at_secondary: Option<String>,
    total_tokens: Option<i64>,
    tokens_per_hour: Option<f64>,
    last_refresh_at: Option<String>,
    plan: Option<String>,
    plan_type: Option<String>,
    #[serde(rename = "plan_type")]
    plan_type_snake: Option<String>,
}

fn bounded_accounts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Row>>, D::Error> {
    deserializer.deserialize_option(OptionVisitor)
}

struct OptionVisitor;
impl<'de> Visitor<'de> for OptionVisitor {
    type Value = Option<Vec<Row>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an account array")
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(BoundedRows).map(Some)
    }
}
struct BoundedRows;
impl<'de> Visitor<'de> for BoundedRows {
    type Value = Vec<Row>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of at most 128 accounts")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while out.len() < MAX_ACCOUNTS {
            match seq.next_element()? {
                Some(value) => out.push(value),
                None => return Ok(out),
            }
        }
        if seq.next_element::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::custom("account count exceeded"));
        }
        Ok(out)
    }
}

fn timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
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

fn normalize_status(raw: &str) -> String {
    let token = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let normalized = match token.as_str() {
        "active" | "enabled" | "healthy" | "logged_in" | "ok" => "ok",
        "disabled" | "inactive" | "suspended" => "paused",
        "auth_required" | "auth_expired" | "login_required" | "reauth" | "reauthrequired"
        | "token_expired" | "unauthorized" => "reauth_required",
        "ratelimited" => "rate_limited",
        "" => "unavailable",
        _ => token.as_str(),
    };
    if normalized.len() > 64
        || !normalized
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        "unavailable".into()
    } else {
        normalized.into()
    }
}

fn account_window(
    pct: Option<f64>,
    reset_at: Option<&str>,
) -> Result<Option<AccountWindow>, Error> {
    pct.map(|pct| {
        if !pct.is_finite() {
            return Err(Error::Invalid);
        }
        Ok(AccountWindow {
            used_pct: pct.clamp(0.0, 1.0),
            resets_at: timestamp(reset_at).map(format_time),
        })
    })
    .transpose()
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountExport {
    pub accounts: Vec<Account>,
    pub updated_at: Option<String>,
    pub source_time: DateTime<Utc>,
}

/// Parse a complete export, applying the transport privacy allowlist. Codex
/// `email` is always empty; `display_name` uses only the approved alias or a
/// numbered fallback. `mtime` is the safe file reader's timestamp, used when
/// the payload timestamp is absent/invalid. Far-future source times use `now`.
pub fn parse_account_export(
    raw: &[u8],
    now: DateTime<Utc>,
    mtime: DateTime<Utc>,
) -> Result<AccountExport, Error> {
    if raw.len() > MAX_BODY {
        return Err(Error::Limit);
    }
    if raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::Invalid);
    }
    let source: Export = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    if source.schema_version != 1 {
        return Err(Error::Invalid);
    }
    let rows = source.accounts.ok_or(Error::Invalid)?;
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::with_capacity(rows.len());
    for row in rows {
        if row.number <= 0 || !seen.insert(row.number) {
            return Err(Error::Invalid);
        }
        let _ = (row.account_id, row.email, row.display_name);
        let alias = row.alias.as_deref().unwrap_or("");
        let label = if alias.trim().is_empty() {
            format!("Account {}", row.number)
        } else {
            safe_label(alias)
        };
        let status = normalize_status(row.status.as_deref().unwrap_or(""));
        if row.tokens_per_hour.is_some_and(|v| !v.is_finite()) {
            return Err(Error::Invalid);
        }
        let plan = [row.plan, row.plan_type, row.plan_type_snake]
            .into_iter()
            .flatten()
            .find(|v| !v.trim().is_empty())
            .unwrap_or_default();
        accounts.push(Account {
            number: row.number,
            email: String::new(),
            display_name: label,
            active: status == "ok",
            status,
            five_hour: account_window(row.five_hour_pct, row.reset_at_primary.as_deref())?,
            seven_day: account_window(row.seven_day_pct, row.reset_at_secondary.as_deref())?,
            tokens_per_hour: row.tokens_per_hour,
            total_tokens: row.total_tokens,
            last_refresh_at: timestamp(row.last_refresh_at.as_deref()).map(format_time),
            plan_type: normalize_plan(&plan),
        });
    }
    let mut source_time = timestamp(source.accounts_updated_at.as_deref()).unwrap_or(mtime);
    if source_time > now + Duration::minutes(5) {
        source_time = now;
    }
    Ok(AccountExport {
        updated_at: (!accounts.is_empty()).then(|| format_time(source_time)),
        accounts,
        source_time,
    })
}

/// Go's 30-minute last-good window, measured from source time (inclusive).
pub fn is_fresh(source_time: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(source_time) <= Duration::minutes(30)
}

#[cfg(test)]
#[path = "codex_accounts_tests.rs"]
mod tests;
