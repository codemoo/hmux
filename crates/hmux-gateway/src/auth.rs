//! Pure Go v1 authentication codecs and cryptographic operations.
//! Callers must supply admission, transaction locks, private-file IO and revocation.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::marker::PhantomData;
use std::net::IpAddr;
use subtle::ConstantTimeEq;

pub const PASSWORD_ITERATIONS: u32 = 600_000;
pub const COOKIE_NAME: &str = "__Host-hmux";
pub const SESSION_FILE_VERSION: i32 = 1;
pub const SESSION_FILE_LIMIT: usize = 128 << 10;
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 8;
pub const LOGIN_LIFETIME_SECONDS: i64 = 7 * 24 * 60 * 60;

// Go's struct decoder requires an object. This guard also covers nested rows.
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

macro_rules! auth_object {
    ($name:ident ($wire:ident) { $( $(#[$field_attr:meta])* pub $field:ident: $type:ty, )* }) => {
        #[derive(Clone, Default, PartialEq, Eq, Serialize)]
        pub struct $name { $( $(#[$field_attr])* pub $field: $type, )* }
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct $wire { $( $(#[$field_attr])* $field: $type, )* }
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

mod go_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        bytes: &Vec<u8>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

auth_object! { Credentials (CredentialsWire) {
    #[serde(default)]
    pub username: String,
    #[serde(default, with = "go_bytes")]
    pub salt: Vec<u8>,
    #[serde(default, with = "go_bytes")]
    pub hash: Vec<u8>,
    #[serde(default)]
    pub totp_secret: String,
    #[serde(default)]
    pub last_step: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub totp_disabled: bool,
} }

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("redacted", &true)
            .finish()
    }
}

impl Credentials {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.username.is_empty()
            || self.salt.len() != 32
            || self.hash.len() != 32
            || decode_totp_secret(&self.totp_secret).is_none()
        {
            return Err("invalid credentials file");
        }
        Ok(())
    }

    /// Go json.Marshal field order, byte encoding and HTML-safe string escaping.
    pub fn go_json(&self) -> String {
        let mut result = format!(
            "{{\"username\":{},\"salt\":{},\"hash\":{},\"totp_secret\":{},\"last_step\":{}",
            go_string(&self.username),
            go_string(&STANDARD.encode(&self.salt)),
            go_string(&STANDARD.encode(&self.hash)),
            go_string(&self.totp_secret),
            self.last_step
        );
        if self.totp_disabled {
            result.push_str(",\"totp_disabled\":true");
        }
        result.push('}');
        result
    }

    /// LastStep is deliberately excluded, so TOTP replay advances do not revoke logins.
    pub fn fingerprint_input(&self) -> String {
        let mut result = format!(
            "{{\"username\":{},\"salt\":{},\"hash\":{},\"totp_secret\":{}",
            go_string(&self.username),
            go_string(&STANDARD.encode(&self.salt)),
            go_string(&STANDARD.encode(&self.hash)),
            go_string(&self.totp_secret)
        );
        if self.totp_disabled {
            result.push_str(",\"totp_disabled\":true");
        }
        result.push('}');
        result
    }

    pub fn fingerprint(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.fingerprint_input().as_bytes()))
    }

    pub fn matches_password(&self, password: &str) -> bool {
        self.hash.len() == 32
            && derive_password(password, &self.salt)
                .ct_eq(&self.hash)
                .into()
    }

    pub fn match_code(&self, code: &str, unix_seconds: i64) -> Option<i64> {
        let secret = decode_totp_secret(&self.totp_secret)?;
        if code.len() != 6 || !code.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // Go integer division truncates toward zero, including pre-epoch values.
        let base_step = unix_seconds / 30;
        for delta in -1..=1 {
            let step = base_step.checked_add(delta)?;
            let mut mac = Hmac::<Sha1>::new_from_slice(&secret).ok()?;
            mac.update(&(step as u64).to_be_bytes());
            let digest = mac.finalize().into_bytes();
            let offset = (digest[19] & 15) as usize;
            let number = (u32::from_be_bytes(digest[offset..offset + 4].try_into().ok()?)
                & 0x7fff_ffff)
                % 1_000_000;
            let expected = format!("{number:06}");
            if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) {
                return Some(step);
            }
        }
        None
    }

    pub fn matches_unused_code(&self, code: &str, unix_seconds: i64) -> Option<i64> {
        self.match_code(code, unix_seconds)
            .filter(|step| *step > self.last_step)
    }
}

pub fn derive_password(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PASSWORD_ITERATIONS, &mut hash);
    hash
}

pub fn decode_totp_secret(value: &str) -> Option<[u8; 20]> {
    let bytes = data_encoding::BASE32_NOPAD.decode(value.as_bytes()).ok()?;
    bytes.try_into().ok()
}

pub fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

pub fn csrf_token(token: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"hmux csrf\0");
    digest.update(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

pub fn valid_token(value: &str) -> bool {
    hmux_core::token::valid(value)
}

pub fn account_profile(username: &str) -> String {
    format!("{:x}", Sha256::digest(username.as_bytes()))
}

pub fn usage_preference_key(username: &str, profile: &str) -> String {
    format!("{:x}", usage_preference_digest(username, profile))
}

pub(crate) fn usage_preference_digest(
    username: &str,
    profile: &str,
) -> sha2::digest::Output<Sha256> {
    let mut digest = Sha256::new();
    digest.update(username.as_bytes());
    digest.update([0]);
    digest.update(profile.as_bytes());
    digest.finalize()
}

fn go_string(value: &str) -> String {
    serde_json::to_string(value)
        .expect("string serialization cannot fail")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

auth_object! { PersistedSession (PersistedSessionWire) {
    #[serde(default)]
    pub token_hash: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub credential_fingerprint: String,
    #[serde(default)]
    pub browser: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_seen_at: String,
    #[serde(default)]
    pub expires_at: String,
} }

impl fmt::Debug for PersistedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedSession")
            .field("redacted", &true)
            .finish()
    }
}

auth_object! { PersistedSessionFile (PersistedSessionFileWire) {
    #[serde(default)]
    pub version: i32,
    #[serde(default)]
    pub sessions: Option<Vec<PersistedSession>>,
} }

impl fmt::Debug for PersistedSessionFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedSessionFile")
            .field("version", &self.version)
            .field(
                "session_count",
                &self.sessions.as_ref().map(Vec::len).unwrap_or(0),
            )
            .finish()
    }
}

impl PersistedSessionFile {
    /// Matches Go json.Marshal's field order and escaping for version 1 files.
    pub fn go_json(&self) -> String {
        let mut result = format!("{{\"version\":{},\"sessions\":", self.version);
        let Some(sessions) = &self.sessions else {
            result.push_str("null}");
            return result;
        };
        result.push('[');
        for (index, row) in sessions.iter().enumerate() {
            if index != 0 {
                result.push(',');
            }
            result.push_str(&format!(
                "{{\"token_hash\":{},\"id\":{},\"username\":{},\"profile\":{},\"credential_fingerprint\":{},\"browser\":{},\"ip\":{},\"created_at\":{},\"last_seen_at\":{},\"expires_at\":{}}}",
                go_string(&row.token_hash), go_string(&row.id), go_string(&row.username),
                go_string(&row.profile), go_string(&row.credential_fingerprint),
                go_string(&row.browser), go_string(&row.ip), go_string(&row.created_at),
                go_string(&row.last_seen_at), go_string(&row.expires_at)
            ));
        }
        result.push_str("]}");
        result
    }
}

#[derive(PartialEq, Eq)]
pub struct ValidatedSessions {
    pub retained: Vec<PersistedSession>,
    pub dirty: bool,
}

impl fmt::Debug for ValidatedSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidatedSessions")
            .field("retained_count", &self.retained.len())
            .field("dirty", &self.dirty)
            .finish()
    }
}

fn parse_go_time(value: &str) -> Result<DateTime<Utc>, &'static str> {
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.with_timezone(&Utc))
        .map_err(|_| "invalid session time")
}

fn valid_session_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
}

/// Validates Go's persisted-session shape, then filters expired or credential-stale rows.
/// `accounts` maps username to its authoritative (profile, credentials).
pub fn validate_session_file(
    file: &PersistedSessionFile,
    now: DateTime<Utc>,
    accounts: &HashMap<String, (String, Credentials)>,
) -> Result<ValidatedSessions, &'static str> {
    if file.version != SESSION_FILE_VERSION {
        return Err("invalid session file");
    }
    let mut ids = HashSet::new();
    let mut tokens = HashSet::new();
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut retained = Vec::new();
    let mut dirty = false;
    for row in file.sessions.as_deref().unwrap_or(&[]) {
        let hash = URL_SAFE_NO_PAD
            .decode(&row.token_hash)
            .map_err(|_| "invalid token hash")?;
        let id = URL_SAFE_NO_PAD
            .decode(&row.id)
            .map_err(|_| "invalid login id")?;
        let created = parse_go_time(&row.created_at)?;
        let seen = parse_go_time(&row.last_seen_at)?;
        let expires = parse_go_time(&row.expires_at)?;
        if hash.len() != 32
            || id.len() != 32
            || !ids.insert(row.id.as_str())
            || !valid_session_text(&row.browser, 80)
            || (row.ip != "unknown" && row.ip.parse::<IpAddr>().is_err())
            || (created.year() == 1
                && created.month() == 1
                && created.day() == 1
                && created.hour() == 0
                && created.minute() == 0
                && created.second() == 0
                && created.nanosecond() == 0)
            || created.checked_add_signed(Duration::seconds(LOGIN_LIFETIME_SECONDS))
                != Some(expires)
            || seen < created
            || seen > expires
            || created > now + Duration::minutes(5)
        {
            return Err("invalid session file");
        }
        let Some((profile, credentials)) = accounts.get(&row.username) else {
            dirty = true;
            continue;
        };
        if row.profile != *profile
            || row.credential_fingerprint != credentials.fingerprint()
            || now >= expires
        {
            dirty = true;
            continue;
        }
        let count = counts.entry((&row.username, &row.profile)).or_default();
        *count += 1;
        if *count > MAX_SESSIONS_PER_ACCOUNT || !tokens.insert(hash) {
            return Err("invalid session file");
        }
        retained.push(row.clone());
    }
    Ok(ValidatedSessions { retained, dirty })
}

pub fn parse_credentials(raw: &[u8]) -> Result<Credentials, &'static str> {
    if raw.len() > 4096 {
        return Err("credentials too large");
    }
    // Go's encoding/json decoder only accepts an object for a struct target.
    require_json_object(raw)?;
    let credentials: Credentials =
        serde_json::from_slice(raw).map_err(|_| "invalid credentials JSON")?;
    credentials.validate()?;
    Ok(credentials)
}

pub fn parse_session_file(raw: &[u8]) -> Result<PersistedSessionFile, &'static str> {
    if raw.len() > SESSION_FILE_LIMIT {
        return Err("session file too large");
    }
    require_json_object(raw)?;
    serde_json::from_slice(raw).map_err(|_| "invalid session JSON")
}

fn require_json_object(raw: &[u8]) -> Result<(), &'static str> {
    match raw
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
    {
        Some(b'{') => Ok(()),
        _ => Err("JSON object required"),
    }
}
