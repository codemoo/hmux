//! Pure, bounded provider-response normalization. Raw extras/identity never
//! become public snapshot fields. Network and credentials belong to Home.
use crate::{model::*, transport, Error};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;

pub const MAX_RESPONSE_BYTES: usize = 64 << 10;

#[derive(Default, Deserialize)]
#[serde(default)]
struct ClaudeResponse<'a> {
    five_hour: Option<ClaudeWindow>,
    seven_day: Option<ClaudeWindow>,
    seven_day_sonnet: Option<ClaudeWindow>,
    seven_day_opus: Option<ClaudeWindow>,
    #[serde(borrow)]
    extra_rate_windows: Option<&'a RawValue>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ClaudeWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CodexResponse<'a> {
    #[serde(borrow)]
    primary: Option<CodexWindow<'a>>,
    secondary: Option<CodexWindow<'a>>,
    tertiary: Option<CodexWindow<'a>>,
    rate_limit: Option<RateLimit<'a>>,
    credits: Option<Credits<'a>>,
    plan_type: Option<String>,
    #[serde(rename = "loginMethod")]
    _login_method: Option<String>,
    #[serde(rename = "accountEmail")]
    _account_email: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct RateLimit<'a> {
    #[serde(borrow)]
    primary_window: Option<CodexWindow<'a>>,
    secondary_window: Option<CodexWindow<'a>>,
    tertiary_window: Option<CodexWindow<'a>>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Credits<'a> {
    #[serde(borrow)]
    remaining: Option<&'a RawValue>,
    balance: Option<&'a RawValue>,
    #[serde(rename = "updatedAt")]
    _updated_at: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct CodexWindow<'a> {
    #[serde(borrow, rename = "usedPercent")]
    used_percent_camel: Option<&'a RawValue>,
    used_percent: Option<&'a RawValue>,
    #[serde(rename = "resetsAt")]
    resets_at: Option<String>,
    reset_at: Option<&'a RawValue>,
    #[serde(rename = "windowMinutes")]
    window_minutes: Option<&'a RawValue>,
    limit_window_seconds: Option<&'a RawValue>,
}

// Match Go's flexible numeric fields: malformed types are missing, numeric
// overflow/nonfinite is an invalid response. Do not allocate arbitrary trees.
fn flex(raw: Option<&RawValue>) -> Result<Option<f64>, Error> {
    let Some(raw) = raw else { return Ok(None) };
    let s = raw.get().trim();
    let s = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s);
    match s.parse::<f64>() {
        Ok(n) if n.is_finite() => Ok(Some(n)),
        Ok(_) => Err(Error::Invalid),
        Err(_) => Ok(None),
    }
}
fn integer(raw: Option<&RawValue>) -> Result<(), Error> {
    if let Some(n) = flex(raw)? {
        if n > i64::MAX as f64 || n < i64::MIN as f64 {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
fn epoch_token(raw: &RawValue) -> Result<std::borrow::Cow<'_, str>, Error> {
    let s = raw.get();
    let value = if s.starts_with('"') {
        std::borrow::Cow::Owned(serde_json::from_str::<String>(s).map_err(|_| Error::Invalid)?)
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    if !value
        .as_bytes()
        .first()
        .is_some_and(|b| *b == b'-' || b.is_ascii_digit())
        || serde_json::from_str::<&RawValue>(&value).is_err()
    {
        return Err(Error::Invalid);
    }
    Ok(value)
}
fn valid_time(s: &str) -> Option<DateTime<Utc>> {
    parse_time(s).filter(|t| t.timestamp() != -62_135_596_800 || t.timestamp_subsec_nanos() != 0)
}
fn claude_window(w: Option<&ClaudeWindow>, now: DateTime<Utc>) -> Result<Window, Error> {
    let Some(w) = w else {
        return Ok(Window::default());
    };
    let pct = w
        .utilization
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
        .ok_or(Error::Invalid)?;
    let reset = w
        .resets_at
        .as_ref()
        .map(|r| valid_time(r.trim()).ok_or(Error::Invalid))
        .transpose()?;
    Ok(Window {
        used_pct: pct / 100.0,
        remaining_seconds: remaining(reset, now),
        resets_at: w.resets_at.as_ref().map(|s| s.trim().to_owned()),
    })
}
fn codex_window(w: Option<&CodexWindow<'_>>, now: DateTime<Utc>) -> Result<Window, Error> {
    let Some(w) = w else {
        return Ok(Window::default());
    };
    let camel = flex(w.used_percent_camel)?;
    let snake = flex(w.used_percent)?; // Validate even the nonselected alias.
    integer(w.window_minutes)?;
    integer(w.limit_window_seconds)?;
    let pct = camel
        .or(snake)
        .filter(|v| (0.0..=100.0).contains(v))
        .ok_or(Error::Invalid)?;
    let text = w
        .resets_at
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let reset = text
        .map(|s| valid_time(s).ok_or(Error::Invalid))
        .transpose()?;
    let epoch = w
        .reset_at
        .map(|raw| {
            // json.Number accepts quoted numbers too. Reject fractional/nonpositive.
            let s = epoch_token(raw)?;
            let seconds = s.parse::<i64>().map_err(|_| Error::Invalid)?;
            if seconds <= 0 {
                return Err(Error::Invalid);
            }
            DateTime::from_timestamp(seconds, 0)
                .map(Some)
                .ok_or(Error::Invalid)
        })
        .transpose()?
        .flatten();
    let reset = reset.or(epoch);
    Ok(Window {
        used_pct: pct / 100.0,
        remaining_seconds: remaining(reset, now),
        resets_at: reset.map(format_time),
    })
}
fn base(provider: Provider, seq: i64, now: DateTime<Utc>) -> Snapshot {
    let mut s = Snapshot::degraded(provider, seq, now, "ok");
    s.status.stale = false;
    s.status.quota_source = "oauth_api".into();
    s.status.quota_observed_at = Some(format_time(now));
    s
}

pub fn normalize(
    provider: Provider,
    raw: &[u8],
    seq: i64,
    now: DateTime<Utc>,
) -> Result<Snapshot, Error> {
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(Error::Limit);
    }
    let mut s = base(provider, seq, now);
    match provider {
        Provider::Claude => {
            let r: ClaudeResponse<'_> = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
            if r.five_hour.is_none() && r.seven_day.is_none() {
                return Err(Error::Invalid);
            }
            if r.extra_rate_windows
                .is_some_and(|r| !r.get().starts_with('['))
            {
                return Err(Error::Invalid);
            }
            s.rolling_5h = claude_window(r.five_hour.as_ref(), now)?;
            s.weekly = claude_window(r.seven_day.as_ref(), now)?;
            claude_window(r.seven_day_sonnet.as_ref(), now)?;
            claude_window(r.seven_day_opus.as_ref(), now)?;
            s.rolling_5h_observed = r.five_hour.is_some();
            s.weekly_observed = r.seven_day.is_some();
        }
        Provider::Codex => {
            let r: CodexResponse<'_> = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
            let nested = r.rate_limit.as_ref();
            // Check flexible field decoding in all windows, as Go Unmarshal does,
            // including direct fields that override the nested window.
            for w in [
                r.primary.as_ref(),
                r.secondary.as_ref(),
                r.tertiary.as_ref(),
                nested.and_then(|n| n.primary_window.as_ref()),
                nested.and_then(|n| n.secondary_window.as_ref()),
                nested.and_then(|n| n.tertiary_window.as_ref()),
            ]
            .into_iter()
            .flatten()
            {
                flex(w.used_percent_camel)?;
                flex(w.used_percent)?;
                integer(w.window_minutes)?;
                integer(w.limit_window_seconds)?;
                if let Some(raw) = w.reset_at {
                    epoch_token(raw)?;
                }
            }
            if let Some(c) = r.credits {
                flex(c.remaining)?;
                flex(c.balance)?;
            }
            let primary = r
                .primary
                .as_ref()
                .or_else(|| nested.and_then(|n| n.primary_window.as_ref()));
            let secondary = r
                .secondary
                .as_ref()
                .or_else(|| nested.and_then(|n| n.secondary_window.as_ref()));
            let tertiary = r
                .tertiary
                .as_ref()
                .or_else(|| nested.and_then(|n| n.tertiary_window.as_ref()));
            if primary.is_none() && secondary.is_none() {
                return Err(Error::Invalid);
            }
            s.rolling_5h = codex_window(primary, now)?;
            s.weekly = codex_window(secondary, now)?;
            codex_window(tertiary, now)?;
            s.rolling_5h_observed = primary.is_some();
            s.weekly_observed = secondary.is_some();
            s.plan_type = normalize_plan(r.plan_type.as_deref().unwrap_or_default());
        }
    }
    transport::validate(&s)?;
    Ok(s)
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
