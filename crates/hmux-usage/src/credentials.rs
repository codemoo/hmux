//! Pure, bounded parsing of the two provider credential file formats.
//! The caller owns file reads and change detection. This module never refreshes,
//! caches, writes, or sends a credential. Compared with Go, it rejects oversized
//! inputs above 4 MiB, tokens above 16 KiB, account fields above 1 KiB, and
//! unsafe HTTP token/account-ID values. Unknown JSON is streamed and skipped.
//! Ambiguous duplicate Claude fields (including case-folded duplicates) are
//! rejected instead of selecting a different token from Go's last-wins decode.

use crate::{quota_state::AccountKey, Provider};
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::fmt;

const MAX_CREDENTIAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_IDENTITY_BYTES: usize = 1024;

/// Only the access token and identity needed by read-only usage requests remain.
/// Refresh and ID tokens, scopes, and expiry data are deliberately discarded.
pub struct Credential {
    token: String,
    account_id: String,
    account_key: AccountKey,
}

impl Credential {
    pub fn access_token(&self) -> &str {
        &self.token
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn account_key(&self) -> AccountKey {
        self.account_key
    }

    pub fn same_token(&self, other: &Self) -> bool {
        self.token == other.token
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential([redacted])")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    TooLarge,
    InvalidJson,
    MissingToken,
    InvalidHeader,
    AmbiguousField,
}

impl fmt::Debug for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "CredentialError::TooLarge",
            Self::InvalidJson => "CredentialError::InvalidJson",
            Self::MissingToken => "CredentialError::MissingToken",
            Self::InvalidHeader => "CredentialError::InvalidHeader",
            Self::AmbiguousField => "CredentialError::AmbiguousField",
        })
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CredentialError {}

pub fn parse(provider: Provider, raw: &[u8]) -> Result<Credential, CredentialError> {
    if raw.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialError::TooLarge);
    }
    match provider {
        Provider::Claude => {
            let root: ClaudeRoot =
                serde_json::from_slice(raw).map_err(|_| CredentialError::InvalidJson)?;
            if root.ambiguous || root.oauth.as_ref().is_some_and(|oauth| oauth.ambiguous) {
                return Err(CredentialError::AmbiguousField);
            }
            let token = root
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.access.as_deref())
                .unwrap_or_default()
                .trim();
            if token.is_empty() {
                return Err(CredentialError::MissingToken);
            }
            credential(token, "", "")
        }
        Provider::Codex => {
            let root: CodexRoot =
                serde_json::from_slice(raw).map_err(|_| CredentialError::InvalidJson)?;
            if let Some(key) = root
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return credential(key, "", "");
            }
            let token = first_nonempty(&root.tokens.access_snake, &root.tokens.access_camel);
            if token.is_empty() {
                return Err(CredentialError::MissingToken);
            }
            let account_id = first_nonempty(&root.tokens.id_snake, &root.tokens.id_camel);
            let email = first_nonempty(&root.tokens.email_snake, &root.tokens.email_camel);
            credential(token, account_id, email)
        }
    }
}

fn first_nonempty<'a>(first: &'a Option<String>, second: &'a Option<String>) -> &'a str {
    first
        .as_deref()
        .into_iter()
        .chain(second.as_deref())
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

#[derive(Default)]
struct CodexRoot {
    api_key: Option<String>,
    tokens: CodexTokens,
}

impl<'de> Deserialize<'de> for CodexRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RootVisitor;
        impl<'de> Visitor<'de> for RootVisitor {
            type Value = CodexRoot;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Codex credential object or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(CodexRoot::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut root = CodexRoot::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "OPENAI_API_KEY" => root.api_key = map.next_value::<MaybeText>()?.0,
                        "tokens" => root.tokens = map.next_value()?,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(root)
            }
        }
        deserializer.deserialize_any(RootVisitor)
    }
}

#[derive(Default)]
struct CodexTokens {
    access_snake: Option<String>,
    access_camel: Option<String>,
    id_snake: Option<String>,
    id_camel: Option<String>,
    email_snake: Option<String>,
    email_camel: Option<String>,
}

impl<'de> Deserialize<'de> for CodexTokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TokensVisitor;
        impl<'de> Visitor<'de> for TokensVisitor {
            type Value = CodexTokens;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any Codex tokens value")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(CodexTokens::default())
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(CodexTokens::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut tokens = CodexTokens::default();
                while let Some(key) = map.next_key::<String>()? {
                    let field = match key.as_str() {
                        "access_token" => Some(&mut tokens.access_snake),
                        "accessToken" => Some(&mut tokens.access_camel),
                        "account_id" => Some(&mut tokens.id_snake),
                        "accountId" => Some(&mut tokens.id_camel),
                        "account_email" => Some(&mut tokens.email_snake),
                        "accountEmail" => Some(&mut tokens.email_camel),
                        _ => None,
                    };
                    if let Some(field) = field {
                        *field = map.next_value::<MaybeText>()?.0;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(tokens)
            }
        }
        deserializer.deserialize_any(TokensVisitor)
    }
}

struct MaybeText(Option<String>);

impl<'de> Deserialize<'de> for MaybeText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl<'de> Visitor<'de> for TextVisitor {
            type Value = MaybeText;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MaybeText(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(MaybeText(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(MaybeText(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(MaybeText(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(MaybeText(None))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(MaybeText(Some(value.to_owned())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(MaybeText(Some(value)))
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(MaybeText(None))
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(MaybeText(None))
            }
        }
        deserializer.deserialize_any(TextVisitor)
    }
}

#[derive(Default)]
struct ClaudeRoot {
    oauth: Option<ClaudeOauth>,
    ambiguous: bool,
}

impl<'de> Deserialize<'de> for ClaudeRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RootVisitor;
        impl<'de> Visitor<'de> for RootVisitor {
            type Value = ClaudeRoot;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Claude credential object or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ClaudeRoot::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut root = ClaudeRoot::default();
                let mut seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key.eq_ignore_ascii_case("claudeAiOauth") {
                        if seen {
                            root.ambiguous = true;
                            map.next_value::<IgnoredAny>()?;
                        } else {
                            seen = true;
                            root.oauth = Some(map.next_value()?);
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(root)
            }
        }
        deserializer.deserialize_any(RootVisitor)
    }
}

#[derive(Default)]
struct ClaudeOauth {
    access: Option<String>,
    ambiguous: bool,
}

impl<'de> Deserialize<'de> for ClaudeOauth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OauthVisitor;
        impl<'de> Visitor<'de> for OauthVisitor {
            type Value = ClaudeOauth;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Claude OAuth object or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ClaudeOauth::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut oauth = ClaudeOauth::default();
                let mut seen = 0u8;
                while let Some(key) = map.next_key::<String>()? {
                    let field = if key.eq_ignore_ascii_case("accessToken") {
                        1
                    } else if key.eq_ignore_ascii_case("refreshToken") {
                        2
                    } else if key.eq_ignore_ascii_case("expiresAt") {
                        4
                    } else if key.eq_ignore_ascii_case("scopes") {
                        8
                    } else if key.eq_ignore_ascii_case("rateLimitTier") {
                        16
                    } else {
                        0
                    };
                    if field != 0 && seen & field != 0 {
                        oauth.ambiguous = true;
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    seen |= field;
                    match field {
                        1 => oauth.access = map.next_value::<StrictText>()?.0,
                        2 | 16 => {
                            map.next_value::<StrictDiscardString>()?;
                        }
                        4 => {
                            map.next_value::<StrictNumber>()?;
                        }
                        8 => {
                            map.next_value::<StrictScopes>()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(oauth)
            }
        }
        deserializer.deserialize_any(OauthVisitor)
    }
}

struct StrictText(Option<String>);
impl<'de> Deserialize<'de> for StrictText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl Visitor<'_> for TextVisitor {
            type Value = StrictText;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictText(None))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrictText(Some(value.to_owned())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictText(Some(value)))
            }
        }
        deserializer.deserialize_any(TextVisitor)
    }
}

struct StrictDiscardString;
impl<'de> Deserialize<'de> for StrictDiscardString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl Visitor<'_> for TextVisitor {
            type Value = StrictDiscardString;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictDiscardString)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(StrictDiscardString)
            }
            fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(StrictDiscardString)
            }
        }
        deserializer.deserialize_any(TextVisitor)
    }
}

struct StrictNumber;
impl<'de> Deserialize<'de> for StrictNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumberVisitor;
        impl Visitor<'_> for NumberVisitor {
            type Value = StrictNumber;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictNumber)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(StrictNumber)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(StrictNumber)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(StrictNumber)
            }
        }
        deserializer.deserialize_any(NumberVisitor)
    }
}

struct StrictScopes;
impl<'de> Deserialize<'de> for StrictScopes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ScopesVisitor;
        impl<'de> Visitor<'de> for ScopesVisitor {
            type Value = StrictScopes;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an array of strings or null")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictScopes)
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                while seq.next_element::<StrictDiscardString>()?.is_some() {}
                Ok(StrictScopes)
            }
        }
        deserializer.deserialize_any(ScopesVisitor)
    }
}

fn credential(token: &str, account_id: &str, email: &str) -> Result<Credential, CredentialError> {
    if token.len() > MAX_TOKEN_BYTES
        || account_id.len() > MAX_IDENTITY_BYTES
        || email.len() > MAX_IDENTITY_BYTES
    {
        return Err(CredentialError::TooLarge);
    }
    // Neither Authorization nor ChatGPT-Account-Id may contain spaces or
    // non-ASCII bytes. Email is only an identity; allow Unicode, not controls.
    if !token.bytes().all(|b| (b'!'..=b'~').contains(&b))
        || !account_id.bytes().all(|b| (b'!'..=b'~').contains(&b))
        || email.chars().any(char::is_control)
    {
        return Err(CredentialError::InvalidHeader);
    }
    let mut digest = Sha256::new();
    if !account_id.is_empty() {
        digest.update(b"id:");
        digest.update(account_id.as_bytes());
    } else if !email.is_empty() {
        digest.update(b"em:");
        digest.update(email.as_bytes());
    } else {
        digest.update(b"tk:");
        digest.update(&token.as_bytes()[token.len().saturating_sub(8)..]);
    }
    Ok(Credential {
        token: token.to_owned(),
        account_id: account_id.to_owned(),
        account_key: AccountKey::from_digest(digest.finalize().into()),
    })
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
