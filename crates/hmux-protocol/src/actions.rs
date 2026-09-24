//! Typed action bodies and the explicit JSON-v1 boundary.
use crate::protobuf::{types as p, Error};
use bytes::Bytes;
use hmux_model as m;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::{cell::Cell, fmt, mem::size_of};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResponseContext {
    Operation(p::Operation),
    TerminalOpen,
    Empty,
    Upload,
}

fn json<T: for<'de> Deserialize<'de>>(raw: &[u8], max: usize) -> Result<T, Error> {
    if raw.len() > max {
        return Err(Error::Size);
    }
    preflight_json(raw)?;
    serde_json::from_slice(raw).map_err(|_| Error::Fields)
}
// Count a conservative JSON tree before serde allocates DTO vectors. A dense
// four-megabyte array can otherwise expand into many megabytes of Vec capacity.
#[derive(Clone, Copy)]
struct Seed<'a> {
    used: &'a Cell<usize>,
    depth: u8,
}
impl Seed<'_> {
    fn charge<E: de::Error>(&self, n: usize) -> Result<(), E> {
        let next = self
            .used
            .get()
            .checked_add(n)
            .ok_or_else(|| E::custom("action tree limit"))?;
        if next > crate::snapshots::MAX_DECODED_SNAPSHOT_BYTES {
            return Err(E::custom("action tree limit"));
        }
        self.used.set(next);
        Ok(())
    }
    fn child<E: de::Error>(&self) -> Result<Self, E> {
        if self.depth >= 32 {
            return Err(E::custom("action nesting limit"));
        }
        Ok(Self {
            used: self.used,
            depth: self.depth + 1,
        })
    }
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded action JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        self.deserialize(d)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        self.charge(v.len())
    }
    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<(), E> {
        self.visit_str(v)
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<(), E> {
        self.visit_str(&v)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(self.child::<A::Error>()?)?.is_some() {
            self.charge::<A::Error>(2 * size_of::<serde_json::Value>())?;
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            self.charge::<A::Error>(key.len() + 2 * size_of::<serde_json::Value>())?;
            map.next_value_seed(self.child::<A::Error>()?)?;
        }
        Ok(())
    }
}
fn preflight_json(raw: &[u8]) -> Result<(), Error> {
    let used = Cell::new(0);
    let mut d = serde_json::Deserializer::from_slice(raw);
    Seed {
        used: &used,
        depth: 0,
    }
    .deserialize(&mut d)
    .map_err(|_| Error::Size)?;
    d.end().map_err(|_| Error::Fields)
}

fn encode<T: Serialize>(v: &T, max: usize) -> Result<Bytes, Error> {
    struct Bounded {
        bytes: Vec<u8>,
        max: usize,
        full: bool,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, raw: &[u8]) -> std::io::Result<usize> {
            if raw.len() > self.max.saturating_sub(self.bytes.len()) {
                self.full = true;
                return Err(std::io::Error::other("action limit"));
            }
            self.bytes.extend_from_slice(raw);
            Ok(raw.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: Vec::new(),
        max,
        full: false,
    };
    serde_json::to_writer(&mut out, v).map_err(|_| {
        if out.full {
            Error::Size
        } else {
            Error::Fields
        }
    })?;
    Ok(out.bytes.into())
}
fn object(raw: &[u8], max: usize) -> Result<(), Error> {
    if raw.len() > max {
        return Err(Error::Size);
    }
    if raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::Fields);
    }
    Ok(())
}
fn raw_or_null(raw: &[u8]) -> &[u8] {
    if raw.is_empty() {
        b"null"
    } else {
        raw
    }
}
const ACTION_MAX: usize = 16 << 10;
const CONVERSATION_MAX: usize = 2 << 20;
const STAGE_MAX: usize = 64 << 10;
const PROFILES_MAX: usize = crate::wire::MAX_MESSAGE - 512;
const MAX_PROFILE_ITEMS: usize = PROFILES_MAX / 19;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Alias {
    #[serde(default)]
    alias: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Hidden {
    hidden: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OkReply {
    ok: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Workspace {
    #[serde(default)]
    change: Option<m::workspace::Change>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderKey {
    provider: String,
    key: String,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderJob {
    provider: String,
    action: String,
    text: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    label: String,
}
struct ProfilesJson<'a>(&'a [p::Profile]);
impl Serialize for ProfilesJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Entry<'a> {
            id: &'a str,
            label: &'a str,
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&Entry {
                id: &item.id,
                label: &item.label,
            })?;
        }
        seq.end()
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Created {
    id: String,
    created_at: i64,
    reused: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<ProviderStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderStatus {
    id: String,
    label: String,
    installed: bool,
    auth: String,
    profile: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    key_hint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    profile_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Job {
    state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    code: String,
    #[serde(default, skip_serializing_if = "is_false")]
    needs_input: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    log: Vec<String>,
}
fn is_false(v: &bool) -> bool {
    !*v
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    protocol_version: u32,
    request_id: String,
    stage_id: String,
    session: m::SessionIdentity,
    expires_at_unix: i64,
    files: Vec<StageFile>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StageFile {
    index: u32,
    path: String,
    size: i64,
    sha256: String,
}

fn to_session(v: m::SessionIdentity) -> p::Session {
    p::Session {
        id: v.id,
        created_at: v.created_at,
    }
}
fn from_session(v: p::Session) -> m::SessionIdentity {
    m::SessionIdentity {
        id: v.id,
        created_at: v.created_at,
    }
}
pub fn workspace_change_to_proto(v: m::workspace::Change) -> p::WorkspaceChange {
    p::WorkspaceChange {
        operation_id: v.operation_id,
        revision: v.revision,
        base: v.base.into_iter().map(to_session).collect(),
        tabs: v.tabs.into_iter().map(to_session).collect(),
        selected: v.selected.map(to_session),
    }
}
pub fn workspace_change_from_proto(v: p::WorkspaceChange) -> Result<m::workspace::Change, Error> {
    let m = m::workspace::Change {
        operation_id: v.operation_id,
        revision: v.revision,
        base: v.base.into_iter().map(from_session).collect(),
        tabs: v.tabs.into_iter().map(from_session).collect(),
        selected: v.selected.map(from_session),
    };
    if !m.valid() {
        return Err(Error::Fields);
    }
    Ok(m)
}
pub fn workspace_to_proto(v: m::workspace::Snapshot) -> p::WorkspaceSnapshot {
    p::WorkspaceSnapshot {
        conflict: v.conflict,
        applied: v.applied,
        version: v.version,
        initialized: v.initialized,
        revision: v.revision,
        tabs: v.tabs.into_iter().map(to_session).collect(),
        selected: v.selected.map(to_session),
    }
}
pub fn workspace_from_proto(v: p::WorkspaceSnapshot) -> Result<m::workspace::Snapshot, Error> {
    let m = m::workspace::Snapshot {
        conflict: v.conflict,
        applied: v.applied,
        version: v.version,
        initialized: v.initialized,
        revision: v.revision,
        tabs: v.tabs.into_iter().map(from_session).collect(),
        selected: v.selected.map(from_session),
    };
    if !m.valid() {
        return Err(Error::Fields);
    }
    Ok(m)
}
pub fn conversation_to_proto(v: m::Conversation) -> p::ConversationResult {
    p::ConversationResult {
        provider: v.provider,
        session_id: v.session_id,
        created_at: v.created_at,
        status: v.status,
        messages: v.messages.map(|items| p::ConversationMessages {
            items: items
                .into_iter()
                .map(|m| p::ConversationMessage {
                    id: m.id,
                    role: m.role,
                    text: m.text,
                })
                .collect(),
        }),
        truncated: v.truncated,
    }
}
pub fn conversation_from_proto(v: p::ConversationResult) -> m::Conversation {
    m::Conversation {
        provider: v.provider,
        session_id: v.session_id,
        created_at: v.created_at,
        status: v.status,
        messages: v.messages.map(|v| {
            v.items
                .into_iter()
                .map(|m| m::ConversationMessage {
                    id: m.id,
                    role: m.role,
                    text: m.text,
                })
                .collect()
        }),
        truncated: v.truncated,
    }
}

pub fn request_from_json(
    operation: p::Operation,
    raw: &[u8],
) -> Result<p::request::Payload, Error> {
    use p::request::Payload as P;
    Ok(match operation {
        p::Operation::Profiles | p::Operation::Conversation => P::Empty(p::Empty {}),
        p::Operation::Create => {
            object(raw, ACTION_MAX)?;
            let v: Create = json(raw, ACTION_MAX)?;
            P::Create(p::CreateRequest {
                profile: v.profile,
                name: v.name,
            })
        }
        p::Operation::Alias => {
            object(raw, ACTION_MAX)?;
            let v: Alias = json(raw, ACTION_MAX)?;
            P::Alias(p::AliasRequest { alias: v.alias })
        }
        p::Operation::Hidden => {
            object(raw, ACTION_MAX)?;
            let v: Hidden = json(raw, ACTION_MAX)?;
            P::Hidden(p::HiddenRequest { hidden: v.hidden })
        }
        p::Operation::Workspace => {
            object(raw, m::workspace::MAX_BYTES + 32)?;
            let v: Workspace = json(raw, m::workspace::MAX_BYTES + 32)?;
            if v.change.as_ref().is_some_and(|c| !c.valid()) {
                return Err(Error::Fields);
            }
            P::Workspace(p::WorkspaceRequest {
                change: v.change.map(workspace_change_to_proto),
            })
        }
        p::Operation::Providers => {
            if !raw.is_empty() && raw != b"null" {
                object(raw, ACTION_MAX)?;
                let v: serde_json::Map<String, serde_json::Value> = json(raw, ACTION_MAX)?;
                if !v.is_empty() {
                    return Err(Error::Fields);
                }
            }
            P::Empty(p::Empty {})
        }
        p::Operation::ProviderKey => {
            let v: ProviderKey = if raw_or_null(raw) == b"null" {
                ProviderKey::default()
            } else {
                json(raw, ACTION_MAX)?
            };
            P::ProviderKey(p::ProviderKeyRequest {
                provider: v.provider,
                key: v.key,
            })
        }
        p::Operation::ProviderJobStart
        | p::Operation::ProviderJob
        | p::Operation::ProviderJobInput
        | p::Operation::ProviderJobCancel => {
            let v: ProviderJob = if raw_or_null(raw) == b"null" {
                ProviderJob::default()
            } else {
                json(raw, ACTION_MAX)?
            };
            P::ProviderJob(p::ProviderJobRequest {
                provider: v.provider,
                action: v.action,
                text: v.text,
            })
        }
        _ => return Err(Error::Unsupported),
    })
}
fn request_payload_unchecked(v: &p::Request) -> Result<Bytes, Error> {
    use p::request::Payload as P;
    match v.payload.as_ref().ok_or(Error::Fields)? {
        P::Empty(_) => Ok(Bytes::new()),
        P::Create(v) => encode(
            &Create {
                profile: v.profile.clone(),
                name: v.name.clone(),
            },
            ACTION_MAX,
        ),
        P::Alias(v) => encode(
            &Alias {
                alias: v.alias.clone(),
            },
            ACTION_MAX,
        ),
        P::Hidden(v) => encode(&Hidden { hidden: v.hidden }, ACTION_MAX),
        P::Workspace(v) => encode(
            &Workspace {
                change: v
                    .change
                    .clone()
                    .map(workspace_change_from_proto)
                    .transpose()?,
            },
            m::workspace::MAX_BYTES + 32,
        ),
        P::ProviderKey(v) => encode(
            &ProviderKey {
                provider: v.provider.clone(),
                key: v.key.clone(),
            },
            ACTION_MAX,
        ),
        P::ProviderJob(v) => encode(
            &ProviderJob {
                provider: v.provider.clone(),
                action: v.action.clone(),
                text: v.text.clone(),
            },
            ACTION_MAX,
        ),
    }
}
pub fn request_payload(v: &p::Request) -> Result<Bytes, Error> {
    validate_request(v)?;
    request_payload_unchecked(v)
}
pub fn validate_request(v: &p::Request) -> Result<(), Error> {
    use p::request::Payload as P;
    let op = p::Operation::try_from(v.operation).map_err(|_| Error::Unsupported)?;
    let good = matches!(
        (op, v.payload.as_ref()),
        (
            p::Operation::Profiles | p::Operation::Conversation | p::Operation::Providers,
            Some(P::Empty(_))
        ) | (p::Operation::Create, Some(P::Create(_)))
            | (p::Operation::Alias, Some(P::Alias(_)))
            | (p::Operation::Hidden, Some(P::Hidden(_)))
            | (p::Operation::Workspace, Some(P::Workspace(_)))
            | (p::Operation::ProviderKey, Some(P::ProviderKey(_)))
            | (
                p::Operation::ProviderJobStart
                    | p::Operation::ProviderJob
                    | p::Operation::ProviderJobInput
                    | p::Operation::ProviderJobCancel,
                Some(P::ProviderJob(_))
            )
    );
    if !good {
        return Err(Error::Fields);
    }
    if matches!(
        op,
        p::Operation::Alias | p::Operation::Hidden | p::Operation::Conversation
    ) && v.session.is_none()
    {
        return Err(Error::Fields);
    }
    match v.payload.as_ref().unwrap() {
        P::Create(x)
            if x.profile.as_ref().map_or(0, String::len)
                + x.name.as_ref().map_or(0, String::len)
                > ACTION_MAX - 32 =>
        {
            Err(Error::Size)
        }
        P::Alias(x) if x.alias.as_ref().is_some_and(|s| s.len() > ACTION_MAX - 16) => {
            Err(Error::Size)
        }
        P::Workspace(x)
            if x.change
                .as_ref()
                .is_some_and(|c| workspace_change_from_proto(c.clone()).is_err()) =>
        {
            Err(Error::Fields)
        }
        P::ProviderKey(x) if x.provider.len() + x.key.len() > ACTION_MAX => Err(Error::Size),
        P::ProviderJob(x) if x.provider.len() + x.action.len() + x.text.len() > ACTION_MAX => {
            Err(Error::Size)
        }
        _ => Ok(()),
    }
}

fn provider_from_json(raw: &[u8]) -> Result<p::ProviderResult, Error> {
    object(raw, ACTION_MAX)?;
    let v: ProviderResult = json(raw, ACTION_MAX)?;
    Ok(p::ProviderResult {
        providers: v.providers.map(|items| p::ProviderStatuses {
            items: items
                .into_iter()
                .map(|s| p::ProviderStatus {
                    id: s.id,
                    label: s.label,
                    installed: s.installed,
                    auth: s.auth,
                    profile: s.profile,
                    version: s.version,
                    key_hint: s.key_hint,
                    profile_id: s.profile_id,
                })
                .collect(),
        }),
        job: v.job.map(|j| p::ProviderJobStatus {
            state: j.state,
            url: j.url,
            code: j.code,
            needs_input: j.needs_input,
            log: j.log,
        }),
        error: v.error,
    })
}
fn provider_to_json(v: p::ProviderResult) -> ProviderResult {
    ProviderResult {
        providers: v.providers.map(|p| {
            p.items
                .into_iter()
                .map(|s| ProviderStatus {
                    id: s.id,
                    label: s.label,
                    installed: s.installed,
                    auth: s.auth,
                    profile: s.profile,
                    version: s.version,
                    key_hint: s.key_hint,
                    profile_id: s.profile_id,
                })
                .collect()
        }),
        job: v.job.map(|j| Job {
            state: j.state,
            url: j.url,
            code: j.code,
            needs_input: j.needs_input,
            log: j.log,
        }),
        error: v.error,
    }
}
fn stage_from_json(raw: &[u8]) -> Result<p::StageResult, Error> {
    object(raw, STAGE_MAX)?;
    let v: Stage = json(raw, STAGE_MAX)?;
    Ok(p::StageResult {
        protocol_version: v.protocol_version,
        request_id: v.request_id,
        stage_id: v.stage_id,
        session: Some(to_session(v.session)),
        expires_at_unix: v.expires_at_unix,
        files: v
            .files
            .into_iter()
            .map(|f| p::StageFile {
                index: f.index,
                path: f.path,
                size: f.size,
                sha256: f.sha256,
            })
            .collect(),
    })
}
fn stage_to_json(v: p::StageResult) -> Result<Stage, Error> {
    Ok(Stage {
        protocol_version: v.protocol_version,
        request_id: v.request_id,
        stage_id: v.stage_id,
        session: from_session(v.session.ok_or(Error::Fields)?),
        expires_at_unix: v.expires_at_unix,
        files: v
            .files
            .into_iter()
            .map(|f| StageFile {
                index: f.index,
                path: f.path,
                size: f.size,
                sha256: f.sha256,
            })
            .collect(),
    })
}
pub fn response_from_json(
    raw: &[u8],
    context: ResponseContext,
) -> Result<Option<p::response::Result>, Error> {
    use p::response::Result as R;
    Ok(Some(match context {
        ResponseContext::Operation(p::Operation::Profiles) => {
            let v: Vec<Profile> = json(raw, PROFILES_MAX)?;
            if v.len() > MAX_PROFILE_ITEMS {
                return Err(Error::Size);
            }
            R::Profiles(p::ProfilesResult {
                items: v
                    .into_iter()
                    .map(|p| p::Profile {
                        id: p.id,
                        label: p.label,
                    })
                    .collect(),
            })
        }
        ResponseContext::Operation(p::Operation::Create) => {
            object(raw, ACTION_MAX)?;
            let v: Created = json(raw, ACTION_MAX)?;
            R::Created(p::CreatedResult {
                id: v.id,
                created_at: v.created_at,
                reused: v.reused,
            })
        }
        ResponseContext::Operation(p::Operation::Alias | p::Operation::Hidden)
        | ResponseContext::TerminalOpen => {
            object(raw, ACTION_MAX)?;
            let v: OkReply = json(raw, ACTION_MAX)?;
            if !v.ok {
                return Err(Error::Fields);
            }
            R::Ok(p::Empty {})
        }
        ResponseContext::Operation(p::Operation::Conversation) => {
            if raw.len() > CONVERSATION_MAX - 4096 {
                return Err(Error::Size);
            }
            preflight_json(raw)?;
            let v = m::decode_conversation_json(raw).map_err(|_| Error::Fields)?;
            R::Conversation(Box::new(conversation_to_proto(v)))
        }
        ResponseContext::Operation(p::Operation::Workspace) => {
            preflight_json(raw)?;
            let v = m::workspace::Snapshot::decode(raw).map_err(|_| Error::Fields)?;
            R::Workspace(Box::new(workspace_to_proto(v)))
        }
        ResponseContext::Operation(
            p::Operation::Providers
            | p::Operation::ProviderKey
            | p::Operation::ProviderJobStart
            | p::Operation::ProviderJob
            | p::Operation::ProviderJobInput
            | p::Operation::ProviderJobCancel,
        ) => R::Providers(Box::new(provider_from_json(raw)?)),
        ResponseContext::Empty => {
            if !raw.is_empty() {
                return Err(Error::Fields);
            };
            return Ok(None);
        }
        ResponseContext::Upload => R::Staged(Box::new(stage_from_json(raw)?)),
        _ => return Err(Error::Unsupported),
    }))
}
fn response_payload_unchecked(v: &p::Response) -> Result<Bytes, Error> {
    use p::response::Result as R;
    match v.result.as_ref() {
        None => Ok(Bytes::new()),
        Some(R::Profiles(p)) => encode(&ProfilesJson(&p.items), PROFILES_MAX),
        Some(R::Created(v)) => encode(
            &Created {
                id: v.id.clone(),
                created_at: v.created_at,
                reused: v.reused,
            },
            ACTION_MAX,
        ),
        Some(R::Ok(_)) => Ok(Bytes::from_static(br#"{"ok":true}"#)),
        Some(R::Conversation(v)) => encode(
            &conversation_from_proto((**v).clone()),
            CONVERSATION_MAX - 4096,
        ),
        Some(R::Workspace(v)) => encode(
            &workspace_from_proto((**v).clone())?,
            m::workspace::MAX_BYTES,
        ),
        Some(R::Providers(v)) => encode(&provider_to_json((**v).clone()), ACTION_MAX),
        Some(R::Staged(v)) => encode(&stage_to_json((**v).clone())?, STAGE_MAX),
    }
}
pub fn response_payload(v: &p::Response) -> Result<Bytes, Error> {
    validate_response(v)?;
    response_payload_unchecked(v)
}
pub fn response_matches(v: &p::Response, context: ResponseContext) -> bool {
    use p::response::Result as R;
    if !v.error.is_empty() {
        return v.result.is_none();
    }
    matches!(
        (context, v.result.as_ref()),
        (
            ResponseContext::Operation(p::Operation::Profiles),
            Some(R::Profiles(_))
        ) | (
            ResponseContext::Operation(p::Operation::Create),
            Some(R::Created(_))
        ) | (
            ResponseContext::Operation(p::Operation::Alias | p::Operation::Hidden),
            Some(R::Ok(_))
        ) | (
            ResponseContext::Operation(p::Operation::Conversation),
            Some(R::Conversation(_))
        ) | (
            ResponseContext::Operation(p::Operation::Workspace),
            Some(R::Workspace(_))
        ) | (
            ResponseContext::Operation(
                p::Operation::Providers
                    | p::Operation::ProviderKey
                    | p::Operation::ProviderJobStart
                    | p::Operation::ProviderJob
                    | p::Operation::ProviderJobInput
                    | p::Operation::ProviderJobCancel
            ),
            Some(R::Providers(_))
        ) | (ResponseContext::TerminalOpen, Some(R::Ok(_)))
            | (ResponseContext::Empty, None)
            | (ResponseContext::Upload, Some(R::Staged(_)))
    )
}
pub fn validate_response(v: &p::Response) -> Result<(), Error> {
    use p::response::Result as R;
    if !v.error.is_empty() {
        return if v.result.is_none() {
            Ok(())
        } else {
            Err(Error::Fields)
        };
    }
    match v.result.as_ref() {
        None | Some(R::Ok(_)) => Ok(()),
        Some(R::Profiles(x))
            if x.items.len() <= MAX_PROFILE_ITEMS
                && x.items
                    .iter()
                    .all(|p| p.id.len() <= 63 && p.label.len() <= 256)
                && response_retained_bytes(v) <= crate::snapshots::MAX_DECODED_SNAPSHOT_BYTES =>
        {
            Ok(())
        }
        Some(R::Created(x)) if x.created_at > 0 && m::validate_session_id(&x.id).is_ok() => Ok(()),
        Some(R::Conversation(x))
            if x.messages.as_ref().is_none_or(|m| {
                m.items.len() <= 200
                    && m.items.iter().map(|i| i.text.len()).sum::<usize>() <= 512 << 10
            }) && response_retained_bytes(v) <= CONVERSATION_MAX =>
        {
            Ok(())
        }
        Some(R::Workspace(x)) if workspace_from_proto((**x).clone()).is_ok() => Ok(()),
        Some(R::Providers(x))
            if x.providers.as_ref().is_none_or(|p| p.items.len() <= 16)
                && x.job.as_ref().is_none_or(|j| j.log.len() <= 12)
                && response_retained_bytes(v) <= ACTION_MAX * 2 =>
        {
            Ok(())
        }
        Some(R::Staged(x))
            if x.protocol_version == 1
                && x.session
                    .as_ref()
                    .is_some_and(|s| m::validate_session_id(&s.id).is_ok() && s.created_at > 0)
                && x.files.len() <= 16
                && x.files.iter().enumerate().all(|(i, f)| {
                    f.index as usize == i
                        && f.size > 0
                        && f.size <= 32 << 20
                        && f.path.len() <= 4096
                        && f.sha256.len() == 64
                })
                && response_retained_bytes(v) <= STAGE_MAX * 2 =>
        {
            Ok(())
        }
        _ => Err(Error::Fields),
    }
}
/// Conservative charge for retained typed reply allocations. Includes spare Vec
/// capacity, every owned String capacity and boxed result storage.
pub fn response_retained_bytes(v: &p::Response) -> usize {
    use p::response::Result as R;
    use std::mem::size_of;
    let s = |s: &String| s.capacity();
    let mut n = size_of::<p::Response>() + s(&v.id) + s(&v.error);
    match v.result.as_ref() {
        Some(R::Profiles(x)) => {
            n += x.items.capacity() * size_of::<p::Profile>();
            for i in &x.items {
                n += s(&i.id) + s(&i.label)
            }
        }
        Some(R::Created(x)) => n += s(&x.id),
        Some(R::Conversation(x)) => {
            n += size_of::<p::ConversationResult>()
                + s(&x.provider)
                + s(&x.session_id)
                + s(&x.status);
            if let Some(m) = &x.messages {
                n += size_of::<p::ConversationMessages>()
                    + m.items.capacity() * size_of::<p::ConversationMessage>();
                for i in &m.items {
                    n += s(&i.id) + s(&i.role) + s(&i.text)
                }
            }
        }
        Some(R::Workspace(x)) => {
            n += size_of::<p::WorkspaceSnapshot>()
                + s(&x.conflict)
                + x.applied.capacity() * size_of::<String>()
                + x.tabs.capacity() * size_of::<p::Session>();
            for i in &x.applied {
                n += s(i)
            }
            for i in &x.tabs {
                n += s(&i.id)
            }
            if let Some(i) = &x.selected {
                n += size_of::<p::Session>() + s(&i.id)
            }
        }
        Some(R::Providers(x)) => {
            n += size_of::<p::ProviderResult>();
            if let Some(p) = &x.providers {
                n += size_of::<p::ProviderStatuses>()
                    + p.items.capacity() * size_of::<p::ProviderStatus>();
                for i in &p.items {
                    n += s(&i.id)
                        + s(&i.label)
                        + s(&i.auth)
                        + s(&i.version)
                        + s(&i.key_hint)
                        + s(&i.profile_id)
                }
            }
            if let Some(j) = &x.job {
                n += size_of::<p::ProviderJobStatus>()
                    + s(&j.state)
                    + s(&j.url)
                    + s(&j.code)
                    + j.log.capacity() * size_of::<String>();
                for i in &j.log {
                    n += s(i)
                }
            }
            if let Some(e) = &x.error {
                n += s(e)
            }
        }
        Some(R::Staged(x)) => {
            n += size_of::<p::StageResult>()
                + s(&x.request_id)
                + s(&x.stage_id)
                + x.files.capacity() * size_of::<p::StageFile>();
            if let Some(i) = &x.session {
                n += size_of::<p::Session>() + s(&i.id)
            }
            for i in &x.files {
                n += s(&i.path) + s(&i.sha256)
            }
        }
        _ => {}
    }
    n
}
