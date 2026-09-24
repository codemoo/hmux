use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_SNAPSHOT_BYTES: usize = 1 << 20;
pub const MAX_ACCOUNTS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}
impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub used_pct: f64,
    pub remaining_seconds: i64,
    pub resets_at: Option<String>,
}
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountWindow {
    pub used_pct: f64,
    pub resets_at: Option<String>,
}
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Account {
    pub number: i64,
    pub email: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    pub active: bool,
    pub status: String,
    pub five_hour: Option<AccountWindow>,
    pub seven_day: Option<AccountWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_hour: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_at: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plan_type: String,
}
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Status {
    pub state: String,
    pub data_source: String,
    pub quota_source: String,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_observed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub schema: i64,
    pub seq: i64,
    pub generated_at_utc: String,
    pub provider: Provider,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plan_type: String,
    pub burn_rate_per_min: f64,
    pub burn_state: String,
    pub today_total_tokens: i64,
    pub today_sessions: i64,
    pub rolling_5h: Window,
    pub weekly: Window,
    pub rolling_5h_observed: bool,
    pub weekly_observed: bool,
    pub status: Status,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<Account>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts_updated_at: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, Snapshot>,
}
impl Snapshot {
    pub fn degraded(provider: Provider, seq: i64, now: DateTime<Utc>, state: &str) -> Self {
        Self {
            schema: 1,
            seq,
            generated_at_utc: format_time(now),
            provider,
            plan_type: String::new(),
            burn_rate_per_min: 0.0,
            burn_state: "idle".into(),
            today_total_tokens: 0,
            today_sessions: 0,
            rolling_5h: Window::default(),
            weekly: Window::default(),
            rolling_5h_observed: false,
            weekly_observed: false,
            status: Status {
                state: state.into(),
                data_source: "api_only".into(),
                quota_source: "none".into(),
                stale: true,
                ..Status::default()
            },
            accounts: Vec::new(),
            accounts_updated_at: None,
            sources: BTreeMap::new(),
        }
    }
}
pub fn format_time(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}
pub fn remaining(reset: Option<DateTime<Utc>>, now: DateTime<Utc>) -> i64 {
    reset.map(|t| (t - now).num_seconds().max(0)).unwrap_or(0)
}
pub fn normalize_plan(raw: &str) -> String {
    let value = raw
        .trim()
        .to_ascii_lowercase()
        .replace("chatgpt_", "")
        .replace("chatgpt-", "");
    match value.as_str() {
        "free" | "go" | "plus" | "pro" | "team" | "business" | "enterprise" | "edu" => value,
        _ => String::new(),
    }
}
pub fn safe_label(raw: &str) -> String {
    let value = raw.trim();
    if value.len() > 256
        || value.chars().any(|c| {
            c.is_control()
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
        })
    {
        String::new()
    } else {
        value.into()
    }
}
