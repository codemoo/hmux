//! Typed Home and browser-facing model contracts. This crate performs no I/O.

use chrono::DateTime;
use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;

// A shared map-only visitor keeps direct serde decoding aligned with Go's
// struct decoder. It also permits JSON null to produce the Go zero value.
fn object_wire<'de, D, W>(deserializer: D) -> Result<Option<W>, D::Error>
where
    D: Deserializer<'de>,
    W: Deserialize<'de>,
{
    struct ObjectVisitor<W>(PhantomData<W>);
    impl<'de, W: Deserialize<'de>> Visitor<'de> for ObjectVisitor<W> {
        type Value = Option<W>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a map or null")
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            W::deserialize(MapAccessDeserializer::new(map)).map(Some)
        }
    }
    deserializer.deserialize_any(ObjectVisitor(PhantomData))
}

macro_rules! object_model {
    (
        $(#[$derive:meta])*
        pub struct $name:ident ($wire:ident) {
            $( $(#[$field_attr:meta])* pub $field:ident: $type:ty, )*
        }
    ) => {
        $(#[$derive])*
        #[derive(Serialize)]
        pub struct $name {
            $( $(#[$field_attr])* pub $field: $type, )*
        }

        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct $wire {
            $( $(#[$field_attr])* $field: $type, )*
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ok(match object_wire::<D, $wire>(deserializer)? {
                    Some(wire) => Self { $( $field: wire.$field, )* },
                    None => Self::default(),
                })
            }
        }
    };
}

pub mod workspace;

pub const SCHEMA_VERSION: i64 = 1;
pub const PROTOCOL_VERSION: i64 = 1;
pub const MAXIMUM_HOST_MEMORY_BYTES: u64 = 1 << 60;

fn is_false(value: &bool) -> bool {
    !*value
}
fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}
fn option_vec_is_empty<T>(value: &Option<Vec<T>>) -> bool {
    value.as_ref().is_none_or(Vec::is_empty)
}
fn go_zero_time() -> String {
    "0001-01-01T00:00:00Z".into()
}

fn decode_go_time<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let Some(value) = Option::<String>::deserialize(d)? else {
        return Ok(go_zero_time());
    };
    DateTime::parse_from_rfc3339(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Inventory (InventoryWire) {
        pub schema_version: i64,
        pub revision: String,
        pub profiles: Option<Vec<Profile>>,
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Profile (ProfileWire) {
        pub id: String,
        pub label: String,
        pub default_directory: String,
        pub command: Option<Vec<String>>,
        pub tags: Option<Vec<String>>,
    }
}

impl Inventory {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!("schema_version must be {SCHEMA_VERSION}"));
        }
        if !safe_metadata(&self.revision, 128) {
            return Err("revision is required and must be safe".into());
        }
        let profiles = self.profiles.as_deref().unwrap_or_default();
        if profiles.is_empty() {
            return Err("at least one profile is required".into());
        }
        let mut seen = HashSet::new();
        for profile in profiles {
            validate_stable_id(&profile.id)?;
            if !seen.insert(&profile.id) {
                return Err("duplicate profile id".into());
            }
            if !safe_metadata(&profile.label, 256) {
                return Err("profile label is required".into());
            }
            let command = profile.command.as_deref().unwrap_or_default();
            if command.is_empty() || command[0].is_empty() || command.len() > 64 {
                return Err("invalid profile command".into());
            }
            if command
                .iter()
                .any(|arg| arg.len() > 4096 || has_control(arg))
            {
                return Err("invalid profile command argument".into());
            }
            if profile.default_directory.is_empty()
                || profile.default_directory.len() > 4096
                || has_control(&profile.default_directory)
            {
                return Err("invalid profile default_directory".into());
            }
            let tags = profile.tags.as_deref().unwrap_or_default();
            if tags.len() > 64 || tags.iter().any(|tag| !safe_metadata(tag, 64)) {
                return Err("invalid profile tags".into());
            }
        }
        Ok(())
    }
}

pub fn validate_stable_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_lowercase()
        || bytes[1..]
            .iter()
            .any(|b| !b.is_ascii_lowercase() && !b.is_ascii_digit() && *b != b'-')
    {
        return Err(format!("invalid stable id {id:?}"));
    }
    Ok(())
}

pub fn validate_session_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    if bytes.len() < 2
        || bytes.len() > 13
        || bytes[0] != b'$'
        || !bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(format!("invalid tmux stable session id {id:?}"));
    }
    Ok(())
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub fn has_control(value: &str) -> bool {
    value.chars().any(|c| c.is_control() || is_bidi_control(c))
}

pub fn safe_text(value: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let mut result = String::new();
    for c in value.chars() {
        let c = if c.is_control() || is_bidi_control(c) {
            ' '
        } else {
            c
        };
        if result.len() + c.len_utf8() > max_bytes {
            break;
        }
        result.push(c);
    }
    result.trim().to_owned()
}

fn safe_metadata(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !has_control(value)
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
    pub struct SessionIdentity (SessionIdentityWire) {
        pub id: String,
        pub created_at: i64,
    }
}

object_model! {
    #[derive(Clone, Debug, PartialEq)]
    pub struct Catalog (CatalogWire) {
        pub protocol_version: i64,
        #[serde(default = "go_zero_time", deserialize_with = "decode_go_time")]
        pub generated_at: String,
        pub sessions: Option<Vec<Session>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "decode_optional_metrics"
        )]
        pub host_metrics: Option<HostMetrics>,
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            protocol_version: 0,
            generated_at: go_zero_time(),
            sessions: None,
            host_metrics: None,
        }
    }
}

object_model! {
    #[derive(Clone, Debug, PartialEq)]
    pub struct HostMetrics (HostMetricsWire) {
        #[serde(default = "go_zero_time", deserialize_with = "decode_go_time")]
        pub observed_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cpu_percent: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub gpu_percent: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub memory_used_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub memory_total_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub disk_used_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub disk_total_bytes: Option<u64>,
    }
}

impl Default for HostMetrics {
    fn default() -> Self {
        Self {
            observed_at: go_zero_time(),
            cpu_percent: None,
            gpu_percent: None,
            memory_used_bytes: None,
            memory_total_bytes: None,
            disk_used_bytes: None,
            disk_total_bytes: None,
        }
    }
}

fn decode_optional_metrics<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<HostMetrics>, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    if raw.is_null() {
        return Ok(None);
    }
    if !raw.is_object() {
        return Ok(Some(HostMetrics::default()));
    }
    let metrics = serde_json::from_value::<HostMetrics>(raw).unwrap_or_default();
    Ok(Some(if metrics.validate().is_ok() {
        metrics
    } else {
        HostMetrics::default()
    }))
}

impl HostMetrics {
    pub fn validate(&self) -> Result<(), String> {
        let observed = DateTime::parse_from_rfc3339(&self.observed_at)
            .map_err(|_| "host metrics observed_at is required")?;
        let zero = chrono::NaiveDate::from_ymd_opt(1, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        if observed.naive_utc() == zero || observed.offset().local_minus_utc() != 0 {
            return Err("host metrics observed_at must be nonzero UTC".into());
        }
        if self.cpu_percent.is_none()
            && self.gpu_percent.is_none()
            && self.memory_used_bytes.is_none()
            && self.memory_total_bytes.is_none()
            && self.disk_used_bytes.is_none()
            && self.disk_total_bytes.is_none()
        {
            return Err("host metrics must contain an observation".into());
        }
        for (name, value) in [
            ("cpu_percent", self.cpu_percent),
            ("gpu_percent", self.gpu_percent),
        ] {
            if let Some(value) = value {
                if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                    return Err(format!("host metrics {name} is out of bounds"));
                }
            }
        }
        match (self.memory_used_bytes, self.memory_total_bytes) {
            (None, None) => {}
            (Some(used), Some(total))
                if total > 0 && total <= MAXIMUM_HOST_MEMORY_BYTES && used <= total => {}
            _ => return Err("host metrics memory byte fields are invalid".into()),
        }
        match (self.disk_used_bytes, self.disk_total_bytes) {
            (None, None) => {}
            (Some(used), Some(total)) if total > 0 && total <= 1 << 53 && used <= total => {}
            _ => return Err("host metrics disk byte fields are invalid".into()),
        }
        Ok(())
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct WorkflowSummary (WorkflowSummaryWire) {
        pub running: i64,
        pub waiting_approval: i64,
        pub waiting_input: i64,
        pub completed: i64,
        pub failed: i64,
        pub interrupted: i64,
        pub stale: i64,
        pub updated_at: i64,
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Workflow (WorkflowWire) {
        pub id: String,
        pub source: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub session_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub turn_id: String,
        pub status: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub model: String,
        pub started_at: i64,
        pub updated_at: i64,
        #[serde(default, skip_serializing_if = "is_zero_i64")]
        pub ended_at: i64,
        pub nodes: Option<Vec<WorkflowNode>>,
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct WorkflowNode (WorkflowNodeWire) {
        pub id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub parent_id: String,
        #[serde(rename = "type")]
        pub node_type: String,
        pub provider: String,
        pub status: String,
        pub started_at: i64,
        pub updated_at: i64,
        #[serde(default, skip_serializing_if = "is_zero_i64")]
        pub ended_at: i64,
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Session (SessionWire) {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alias: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<SessionIdentity>,
    pub activity_at: i64,
    #[serde(rename = "attached_clients")]
    pub attached: i64,
    pub window_count: i64,
    pub window_names: Option<Vec<String>>,
    pub active_window: String,
    pub current_path: String,
    pub current_command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "option_vec_is_empty")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub process: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub working_since: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub width: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub height: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowSummary>,
    #[serde(default, skip_serializing_if = "option_vec_is_empty")]
    pub workflows: Option<Vec<Workflow>>,
    #[serde(skip)]
    pub pane_pid: i64,
    }
}

pub const CONVERSATION_READY: &str = "ready";
pub const CONVERSATION_UNAVAILABLE: &str = "unavailable";
pub const CONVERSATION_AMBIGUOUS: &str = "ambiguous";

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Conversation (ConversationWire) {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub provider: String,
        pub session_id: String,
        pub created_at: i64,
        pub status: String,
        pub messages: Option<Vec<ConversationMessage>>,
        pub truncated: bool,
    }
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct ConversationMessage (ConversationMessageWire) {
        pub id: String,
        pub role: String,
        pub text: String,
    }
}

// Object shape is enforced by each public DTO's Deserialize implementation.
// Byte limits remain the transport or store owner's responsibility.
fn decode_object<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, String> {
    serde_json::from_slice(raw).map_err(|e| e.to_string())
}

pub fn decode_inventory_json(raw: &[u8]) -> Result<Inventory, String> {
    decode_object(raw)
}
pub fn decode_catalog_json(raw: &[u8]) -> Result<Catalog, String> {
    decode_object(raw)
}
pub fn decode_conversation_json(raw: &[u8]) -> Result<Conversation, String> {
    decode_object(raw)
}
pub fn decode_session_json(raw: &[u8]) -> Result<Session, String> {
    decode_object(raw)
}
pub fn decode_session_identity_json(raw: &[u8]) -> Result<SessionIdentity, String> {
    decode_object(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_text_match_go_cases() {
        assert!(validate_session_id("$0").is_ok());
        assert!(validate_session_id("$1234567890123").is_err());
        assert!(validate_stable_id("shell-1").is_ok());
        assert!(validate_stable_id("bad;id").is_err());
        assert_eq!(safe_text("한글\u{1b}[31m\tname\n", 100), "한글 [31m name");
        assert_eq!(safe_text("safe\u{202e}evil", 100), "safe evil");
        assert_eq!(safe_text("가나", 4), "가");
    }

    #[test]
    fn metrics_fail_open_only_within_optional_field() {
        let base = r#"{"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[],"host_metrics":VALUE}"#;
        for value in [
            r#""bad""#,
            r#"{"observed_at":"bad","cpu_percent":20}"#,
            r#"{"observed_at":"2026-09-08T12:00:00Z","cpu_percent":100.1}"#,
            r#"{"observed_at":"2026-09-08T12:00:00Z","cpu_percent":20,"host_id":"private"}"#,
        ] {
            let catalog: Catalog = serde_json::from_str(&base.replace("VALUE", value)).unwrap();
            assert!(catalog.host_metrics.unwrap().validate().is_err());
        }
        assert!(serde_json::from_str::<Catalog>(r#"{"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[],"unexpected":true}"#).is_err());
    }
}
