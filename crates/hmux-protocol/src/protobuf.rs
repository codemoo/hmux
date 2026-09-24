//! Candidate v2 Home transport. No network or authentication is implemented here.
//! Preflight field/count/size checks run before prost allocates typed messages.
use crate::wire::{MAX_DATA, MAX_MESSAGE, MAX_UPLOAD_CHUNK};
use bytes::Bytes;
use prost::{
    encoding::{decode_key, decode_varint, WireType},
    Message,
};

pub mod types {
    include!("generated/hmux.v2.rs");
}
pub const SUBPROTOCOL: &str = "hmux-home.pb.v2.controls1";
pub const VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Size,
    Malformed,
    Unsupported,
    Direction,
    Fields,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToHome,
    ToGateway,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negotiated {
    JsonV1,
    ProtobufV2,
}
/// Call only after a successful authenticated HTTP 101. Failed upgrades have no
/// negotiated transport and must not trigger a legacy retry.
pub fn negotiate(selected: Option<&str>) -> Result<Negotiated, Error> {
    match selected {
        None | Some("") => Ok(Negotiated::JsonV1),
        Some(SUBPROTOCOL) => Ok(Negotiated::ProtobufV2),
        Some(_) => Err(Error::Unsupported),
    }
}

#[derive(Clone, Copy)]
enum Schema {
    Envelope,
    Hello,
    Request,
    Response,
    Open,
    Data(usize),
    Resize,
    Reference,
    Ack,
    Catalog,
    CatalogSessions,
    CatalogSession,
    CatalogStrings(usize),
    CatalogMetrics,
    WorkflowSummary,
    Workflows,
    Workflow,
    WorkflowNodes,
    WorkflowNode,
    Usage(bool),
    UsageSource,
    UsageWindow,
    UsageAccountWindow,
    UsageStatus,
    UsageAccount,
    Empty,
    Completion,
    UploadStart,
    UploadHeader,
    File,
    Session,
    CreateRequest,
    AliasRequest,
    HiddenRequest,
    WorkspaceRequest,
    WorkspaceChange,
    ProviderKeyRequest,
    ProviderJobRequest,
    Profile,
    ProfilesResult,
    CreatedResult,
    ConversationResult,
    ConversationMessages,
    ConversationMessage,
    WorkspaceSnapshot,
    ProviderResult,
    ProviderStatuses,
    ProviderStatus,
    ProviderJobStatus,
    StageResult,
    StageFile,
}
#[derive(Clone, Copy)]
enum Field {
    Uint32,
    Uint64,
    Bool,
    Double,
    Int64,
    Enum,
    Text(usize),
    Blob(usize),
    Nested(Schema),
}
fn field(schema: Schema, tag: u32) -> Option<(Field, usize)> {
    use crate::snapshots as s;
    use Field::*;
    use Schema::*;
    let f = match (schema, tag) {
        (Envelope, 1) => Uint32,
        (Envelope, 10) => Nested(Hello),
        (Envelope, 11) => Nested(Request),
        (Envelope, 12 | 20 | 22 | 33 | 34) => Nested(Response),
        (Envelope, 13) => Nested(Open),
        (Envelope, 14) => Nested(Data(MAX_DATA)),
        (Envelope, 15) => Nested(Data(crate::flow::CHUNK)),
        (Envelope, 16) => Nested(Resize),
        (Envelope, 17 | 18 | 21 | 29 | 30 | 31) => Nested(Reference),
        (Envelope, 19 | 32) => Nested(Ack),
        (Envelope, 35) => Nested(Catalog),
        (Envelope, 36) => Nested(Usage(false)),
        (Envelope, 25) => Nested(Empty),
        (Envelope, 26) => Nested(Completion),
        (Envelope, 27) => Nested(UploadStart),
        (Envelope, 28) => Nested(Data(MAX_UPLOAD_CHUNK)),
        (Hello, 1) | (Open, 5) => return Some((Text(64), 16)),
        (
            Request | Response | Open | Data(_) | Resize | Reference | Ack | Completion
            | UploadStart | Session,
            1,
        ) => Text(64),
        (Request, 2) => Enum,
        (Resize | Open, 3) | (Resize, 2) | (Open, 4) => Uint32,
        (Ack | Session, 2) => Int64,
        (Request, 3) | (Open | UploadStart, 2) => Nested(if matches!(schema, UploadStart) {
            UploadHeader
        } else {
            Session
        }),
        (Request, 10) => Nested(Empty),
        (Request, 11) => Nested(CreateRequest),
        (Request, 12) => Nested(AliasRequest),
        (Request, 13) => Nested(HiddenRequest),
        (Request, 14) => Nested(WorkspaceRequest),
        (Request, 15) => Nested(ProviderKeyRequest),
        (Request, 16) => Nested(ProviderJobRequest),
        (Response, 10) => Nested(ProfilesResult),
        (Response, 11) => Nested(CreatedResult),
        (Response, 12) => Nested(Empty),
        (Response, 13) => Nested(ConversationResult),
        (Response, 14) => Nested(WorkspaceSnapshot),
        (Response, 15) => Nested(ProviderResult),
        (Response, 16) => Nested(StageResult),
        (Response, 3) => Text(128),
        (CreateRequest, 1 | 2) | (AliasRequest, 1) => Text(16 << 10),
        (HiddenRequest, 1) => Bool,
        (WorkspaceRequest, 1) => Nested(WorkspaceChange),
        (WorkspaceChange, 1) => Text(80),
        (WorkspaceChange, 2) => Uint64,
        (WorkspaceChange, 3 | 4) => return Some((Nested(Session), 32)),
        (WorkspaceChange, 5) => Nested(Session),
        (ProviderKeyRequest, 1) | (ProviderJobRequest, 1 | 2) => Text(64),
        (ProviderKeyRequest, 2) | (ProviderJobRequest, 3) => Text(16 << 10),
        (Profile, 1 | 2) => Text(256),
        (ProfilesResult, 1) => return Some((Nested(Profile), (MAX_MESSAGE - 512) / 19)),
        (CreatedResult, 1) => Text(64),
        (CreatedResult, 2) => Int64,
        (CreatedResult, 3) => Bool,
        (ConversationResult, 1 | 2 | 4) => Text(64),
        (ConversationResult, 3) => Int64,
        (ConversationResult, 5) => Nested(ConversationMessages),
        (ConversationResult, 6) => Bool,
        (ConversationMessages, 1) => return Some((Nested(ConversationMessage), 200)),
        (ConversationMessage, 1 | 2) => Text(128),
        (ConversationMessage, 3) => Text(512 << 10),
        (WorkspaceSnapshot, 1) => Text(32),
        (WorkspaceSnapshot, 2) => return Some((Text(80), 64)),
        (WorkspaceSnapshot, 3) => Int64,
        (WorkspaceSnapshot, 4) => Bool,
        (WorkspaceSnapshot, 5) => Uint64,
        (WorkspaceSnapshot, 6) => return Some((Nested(Session), 32)),
        (WorkspaceSnapshot, 7) => Nested(Session),
        (ProviderResult, 1) => Nested(ProviderStatuses),
        (ProviderResult, 2) => Nested(ProviderJobStatus),
        (ProviderResult, 3) => Text(128),
        (ProviderStatuses, 1) => return Some((Nested(ProviderStatus), 16)),
        (ProviderStatus, 1 | 2 | 4 | 6 | 7 | 8) => Text(256),
        (ProviderStatus, 3 | 5) => Bool,
        (ProviderJobStatus, 1) => Text(64),
        (ProviderJobStatus, 2 | 3) => Text(1024),
        (ProviderJobStatus, 4) => Bool,
        (ProviderJobStatus, 5) => return Some((Text(1024), 12)),
        (StageResult, 1) => Uint32,
        (StageResult, 2 | 3) => Text(64),
        (StageResult, 4) => Nested(Session),
        (StageResult, 5) => Int64,
        (StageResult, 6) => return Some((Nested(StageFile), 16)),
        (StageFile, 1) => Uint32,
        (StageFile, 2) => Text(4096),
        (StageFile, 3) => Int64,
        (StageFile, 4) => Text(64),
        (Data(max), 2) => Blob(max),
        (Completion, 2) => Nested(Session),
        (Completion, 3) => Text(64),
        (UploadHeader, 1 | 4) | (File, 1) => Uint32,
        (UploadHeader, 5) | (File, 2) => Int64,
        (UploadHeader, 2) => Text(32),
        (UploadHeader, 3) => Nested(Session),
        (UploadHeader, 6) => return Some((Nested(File), 16)),
        (File, 3) => Text(16),
        (Catalog, 1) => Int64,
        (Catalog, 2) => Text(128),
        (CatalogMetrics, 1) => Text(s::MAX_CATALOG_BYTES),
        (Catalog, 3) => Nested(CatalogSessions),
        (Catalog, 4) => Nested(CatalogMetrics),
        (CatalogSessions, 1) => return Some((Nested(CatalogSession), s::MAX_CATALOG_SESSIONS)),
        (CatalogSession, 1 | 5) => Nested(Session),
        (CatalogSession, 4) => Bool,
        (CatalogSession, 6..=8 | 21..=23) => Int64,
        (CatalogSession, 9) => Nested(CatalogStrings(s::MAX_CATALOG_WINDOWS)),
        (CatalogSession, 15) => Nested(CatalogStrings(s::MAX_CATALOG_TAGS)),
        (CatalogSession, 24) => Nested(WorkflowSummary),
        (CatalogSession, 25) => Nested(Workflows),
        (CatalogSession, 2 | 3 | 10..=14 | 16..=20) => Text(s::MAX_CATALOG_TEXT),
        (CatalogStrings(max), 1) => return Some((Text(s::MAX_CATALOG_TEXT), max)),
        (CatalogMetrics, 2 | 3) => Double,
        (CatalogMetrics, 4..=7) => Uint64,
        (WorkflowSummary, 1..=8) => Int64,
        (Workflows, 1) => return Some((Nested(Workflow), s::MAX_WORKFLOWS)),
        (Workflow, 1..=6) | (WorkflowNode, 1..=5) => Text(s::MAX_CATALOG_TEXT),
        (Workflow, 7..=9) | (WorkflowNode, 6..=8) => Int64,
        (Workflow, 10) => Nested(WorkflowNodes),
        (WorkflowNodes, 1) => return Some((Nested(WorkflowNode), s::MAX_WORKFLOW_NODES)),
        (Usage(_), 1 | 2 | 8 | 9) => Int64,
        (Usage(_), 3 | 16) => Text(128),
        (Usage(_), 4) => Enum,
        (Usage(_), 5 | 7) => Text(32),
        (Usage(_), 6) => Double,
        (Usage(_), 10 | 11) => Nested(UsageWindow),
        (Usage(_), 12 | 13) => Bool,
        (Usage(_), 14) => Nested(UsageStatus),
        (Usage(_), 15) => return Some((Nested(UsageAccount), hmux_usage::model::MAX_ACCOUNTS)),
        (Usage(false), 17) => return Some((Nested(UsageSource), 2)),
        (UsageSource, 1) => Text(16),
        (UsageSource, 2) => Nested(Usage(true)),
        (UsageWindow | UsageAccountWindow, 1) | (UsageAccount, 8) => Double,
        (UsageWindow, 2) | (UsageAccount, 1 | 9) => Int64,
        (UsageWindow, 3) | (UsageAccountWindow, 2) => Text(128),
        (UsageStatus, 1) | (UsageAccount, 5) => Text(64),
        (UsageStatus, 2 | 3 | 5 | 6) | (UsageAccount, 10) => Text(128),
        (UsageAccount, 2 | 3) => Text(256),
        (UsageStatus, 4) | (UsageAccount, 4) => Bool,
        (UsageAccount, 6 | 7) => Nested(UsageAccountWindow),
        (UsageAccount, 11) => Text(32),
        _ => return None,
    };
    Some((f, 1))
}
// Conservative transient-tree admission, separate from wire/retained-cache budgets.
// Charge spare Vec capacity and nested structures before prost allocates them.
fn tree_cost(schema: Schema) -> usize {
    use types as p;
    use Schema::*;
    match schema {
        Catalog => size_of::<p::CatalogSnapshot>(),
        CatalogSessions => size_of::<p::CatalogSessions>(),
        CatalogSession => size_of::<p::CatalogSession>(),
        CatalogStrings(_) => size_of::<p::CatalogStrings>(),
        CatalogMetrics => size_of::<p::CatalogHostMetrics>(),
        WorkflowSummary => size_of::<p::CatalogWorkflowSummary>(),
        Workflows => size_of::<p::CatalogWorkflows>(),
        Workflow => size_of::<p::CatalogWorkflow>(),
        WorkflowNodes => size_of::<p::CatalogWorkflowNodes>(),
        WorkflowNode => size_of::<p::CatalogWorkflowNode>(),
        Usage(_) => size_of::<p::UsageSnapshot>(),
        UsageSource => size_of::<p::UsageSource>(),
        UsageWindow => size_of::<p::UsageWindow>(),
        UsageAccountWindow => size_of::<p::UsageAccountWindow>(),
        UsageStatus => size_of::<p::UsageStatus>(),
        UsageAccount => size_of::<p::UsageAccount>(),
        ProfilesResult => size_of::<p::ProfilesResult>(),
        Profile => size_of::<p::Profile>(),
        ConversationResult => size_of::<p::ConversationResult>(),
        ConversationMessages => size_of::<p::ConversationMessages>(),
        ConversationMessage => size_of::<p::ConversationMessage>(),
        WorkspaceSnapshot => size_of::<p::WorkspaceSnapshot>(),
        WorkspaceChange => size_of::<p::WorkspaceChange>(),
        ProviderResult => size_of::<p::ProviderResult>(),
        ProviderStatuses => size_of::<p::ProviderStatuses>(),
        ProviderStatus => size_of::<p::ProviderStatus>(),
        ProviderJobStatus => size_of::<p::ProviderJobStatus>(),
        StageResult => size_of::<p::StageResult>(),
        StageFile => size_of::<p::StageFile>(),
        _ => 0,
    }
}
fn charge(budget: &mut usize, bytes: usize) -> Result<(), Error> {
    *budget = budget.checked_sub(bytes).ok_or(Error::Size)?;
    Ok(())
}
fn scan(mut raw: &[u8], schema: Schema, depth: usize, budget: &mut usize) -> Result<(), Error> {
    if depth > 12 {
        return Err(Error::Fields);
    }
    match schema {
        Schema::Catalog if raw.len() > crate::snapshots::MAX_CATALOG_BYTES => {
            return Err(Error::Size)
        }
        Schema::Usage(_) if raw.len() > crate::snapshots::MAX_USAGE_BYTES => {
            return Err(Error::Size)
        }
        _ => {}
    }
    charge(budget, tree_cost(schema).saturating_mul(2))?;
    let mut counts = [0usize; 37];
    let mut bodies = 0;
    let mut oneof = 0;
    while !raw.is_empty() {
        let (tag, wire) = decode_key(&mut raw).map_err(|_| Error::Malformed)?;
        let (kind, limit) = field(schema, tag).ok_or(Error::Unsupported)?;
        let count = counts.get_mut(tag as usize).ok_or(Error::Fields)?;
        *count = count.checked_add(1).ok_or(Error::Fields)?;
        if *count > limit {
            return Err(Error::Fields);
        }
        if matches!(schema, Schema::Envelope) && tag >= 10 {
            bodies += 1;
            if bodies > 1 {
                return Err(Error::Fields);
            }
        }
        if (matches!(schema, Schema::Request | Schema::Response) && tag >= 10) {
            oneof += 1;
            if oneof > 1 {
                return Err(Error::Fields);
            }
        }
        match kind {
            Field::Uint32 | Field::Uint64 | Field::Int64 | Field::Enum | Field::Bool => {
                if wire != WireType::Varint {
                    return Err(Error::Malformed);
                }
                let value = decode_varint(&mut raw).map_err(|_| Error::Malformed)?;
                // Prost narrows integer fields with casts. Check wire ranges
                // before decode so an oversized version, geometry or enum
                // cannot wrap into an accepted value. Our enums are positive;
                // negative and unknown enum values are unsupported as well.
                if (matches!(kind, Field::Uint32) && value > u32::MAX as u64)
                    || (matches!(kind, Field::Enum) && value > i32::MAX as u64)
                    || (matches!(kind, Field::Bool) && value > 1)
                {
                    return Err(Error::Fields);
                }
            }
            Field::Double => {
                if wire != WireType::SixtyFourBit || raw.len() < 8 {
                    return Err(Error::Malformed);
                }
                raw = &raw[8..];
            }
            Field::Text(_) | Field::Blob(_) | Field::Nested(_) => {
                if wire != WireType::LengthDelimited {
                    return Err(Error::Malformed);
                }
                let length =
                    usize::try_from(decode_varint(&mut raw).map_err(|_| Error::Malformed)?)
                        .map_err(|_| Error::Size)?;
                if length > raw.len() {
                    return Err(Error::Malformed);
                }
                let (part, rest) = raw.split_at(length);
                raw = rest;
                match kind {
                    Field::Text(max) => {
                        charge(
                            budget,
                            length
                                + if limit > 1 {
                                    2 * size_of::<String>()
                                } else {
                                    0
                                },
                        )?;
                        if length > max || std::str::from_utf8(part).is_err() {
                            return Err(Error::Fields);
                        }
                    }
                    Field::Blob(max) => {
                        if length > max {
                            return Err(Error::Size);
                        }
                    }
                    Field::Nested(nested) => scan(part, nested, depth + 1, budget)?,
                    _ => unreachable!(),
                }
            }
        }
    }
    Ok(())
}

/// Decoding owns exactly the supplied WS allocation. Any Bytes slice may retain
/// it, so queue accounting must charge that allocation until every slice is gone.
pub fn decode(raw: Bytes, direction: Direction) -> Result<types::Envelope, Error> {
    if raw.len() > MAX_MESSAGE {
        return Err(Error::Size);
    }
    let mut budget = crate::snapshots::MAX_DECODED_SNAPSHOT_BYTES;
    scan(&raw, Schema::Envelope, 0, &mut budget)?;
    let message = types::Envelope::decode(raw).map_err(|_| Error::Malformed)?;
    validate(&message, direction)?;
    Ok(message)
}
pub fn encode(message: &types::Envelope, direction: Direction) -> Result<Bytes, Error> {
    validate(message, direction)?;
    let length = message.encoded_len();
    if length > MAX_MESSAGE {
        return Err(Error::Size);
    }
    let raw = message.encode_to_vec();
    let mut budget = crate::snapshots::MAX_DECODED_SNAPSHOT_BYTES;
    scan(&raw, Schema::Envelope, 0, &mut budget)?;
    Ok(Bytes::from(raw))
}
fn id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64
}
fn session(value: Option<&types::Session>) -> bool {
    value.is_some_and(|s| {
        s.created_at > 0
            && s.id.strip_prefix('$').is_some_and(|v| {
                (1..=12).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit())
            })
    })
}
fn geometry(cols: u32, rows: u32) -> bool {
    (2..=500).contains(&cols) && (2..=250).contains(&rows)
}
fn caps(values: &[String]) -> bool {
    values.len() <= 16 && values.iter().all(|s| !s.is_empty() && s.len() <= 64)
}
pub(crate) fn validate(message: &types::Envelope, direction: Direction) -> Result<(), Error> {
    use types::envelope::Body::*;
    if message.version != VERSION {
        return Err(Error::Unsupported);
    }
    let body = message.body.as_ref().ok_or(Error::Fields)?;
    let to_gateway = matches!(
        body,
        Hello(_)
            | Response(_)
            | TerminalOutput(_)
            | TerminalExit(_)
            | RefreshResult(_)
            | Catalog(_)
            | Usage(_)
            | UsageUnavailable(_)
            | TaskComplete(_)
            | UploadReady(_)
            | UploadAck(_)
            | UploadComplete(_)
            | UploadError(_)
    );
    if to_gateway != (direction == Direction::ToGateway) {
        return Err(Error::Direction);
    }
    let valid = match body {
        Hello(v) => caps(&v.capabilities),
        Request(v) => {
            id(&v.id)
                && types::Operation::try_from(v.operation)
                    .is_ok_and(|o| o != types::Operation::Unspecified)
                && v.session
                    .as_ref()
                    .is_none_or(|_| session(v.session.as_ref()))
                && crate::actions::validate_request(v).is_ok()
        }
        Response(v) => {
            id(&v.id) && v.error.len() <= 128 && crate::actions::validate_response(v).is_ok()
        }
        TerminalExit(v) | RefreshResult(v) => {
            id(&v.id)
                && v.error.len() <= 128
                && v.result.is_none()
                && crate::actions::validate_response(v).is_ok()
        }
        UploadComplete(v) => {
            id(&v.id)
                && v.error.is_empty()
                && matches!(v.result.as_ref(), Some(types::response::Result::Staged(_)))
                && crate::actions::validate_response(v).is_ok()
        }
        UploadError(v) => {
            id(&v.id) && !v.error.is_empty() && v.error.len() <= 128 && v.result.is_none()
        }
        TerminalOpen(v) => {
            id(&v.id)
                && session(v.session.as_ref())
                && geometry(v.cols, v.rows)
                && caps(&v.capabilities)
        }
        TerminalInput(v) => id(&v.id) && !v.data.is_empty() && v.data.len() <= MAX_DATA,
        TerminalOutput(v) => id(&v.id) && !v.data.is_empty() && v.data.len() <= crate::flow::CHUNK,
        UploadData(v) => id(&v.id) && !v.data.is_empty() && v.data.len() <= MAX_UPLOAD_CHUNK,
        Resize(v) => id(&v.id) && geometry(v.cols, v.rows),
        Close(v) | Cancel(v) | Refresh(v) | UploadFinish(v) | UploadCancel(v) | UploadReady(v) => {
            id(&v.id)
        }
        OutputAck(v) => id(&v.id) && v.received > 0 && v.received <= crate::flow::CHUNK as i64,
        UploadAck(v) => id(&v.id) && v.received >= 0 && v.received <= 128 << 20,
        Catalog(v) => crate::snapshots::validate_catalog(v).is_ok(),
        Usage(v) => crate::snapshots::validate_usage(v).is_ok(),
        UsageUnavailable(_) => true,
        TaskComplete(v) => {
            id(&v.id)
                && session(v.session.as_ref())
                && !v.completed_at.is_empty()
                && v.completed_at.len() <= 64
        }
        UploadStart(v) => {
            id(&v.id)
                && v.header
                    .as_ref()
                    .is_some_and(|h| valid_upload_header(h) && h.request_id == v.id)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Error::Fields)
    }
}

fn valid_upload_header(h: &types::UploadHeader) -> bool {
    let good_session = h.session.as_ref().is_some_and(|s| {
        s.created_at > 0
            && s.id.strip_prefix('$').is_some_and(|v| {
                (1..=31).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit())
            })
    });
    h.protocol_version == 1
        && h.request_id.len() == 32
        && h.request_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && good_session
        && (1..=16).contains(&h.file_count)
        && h.file_count as usize == h.files.len()
        && (1..=128 << 20).contains(&h.total_bytes)
        && h.files.iter().enumerate().all(|(i, f)| {
            f.index as usize == i
                && (1..=32 << 20).contains(&f.size)
                && f.extension.len() <= 16
                && f.extension
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        && h.files
            .iter()
            .try_fold(0i64, |sum, f| sum.checked_add(f.size))
            == Some(h.total_bytes)
}
