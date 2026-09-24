//! Explicit v1 JSON adapter for supported Home operations. Protobuf bodies are
//! the candidate internal form; this adapter does not authorize an operation.
use crate::{
    actions::{self, ResponseContext},
    protobuf::{self as pb, types as p, Direction, Error},
    snapshots, wire as w,
};
use bytes::Bytes;
use serde_json::value::RawValue;

pub fn operation(name: &str) -> Result<p::Operation, Error> {
    use p::Operation::*;
    Ok(match name {
        "profiles" => Profiles,
        "create" => Create,
        "alias" => Alias,
        "hidden" => Hidden,
        "conversation" => Conversation,
        "workspace" => Workspace,
        "providers" => Providers,
        "provider-key" => ProviderKey,
        "provider-job-start" => ProviderJobStart,
        "provider-job" => ProviderJob,
        "provider-job-input" => ProviderJobInput,
        "provider-job-cancel" => ProviderJobCancel,
        _ => return Err(Error::Unsupported),
    })
}
fn operation_name(value: i32) -> Result<&'static str, Error> {
    use p::Operation::*;
    Ok(
        match p::Operation::try_from(value).map_err(|_| Error::Unsupported)? {
            Profiles => "profiles",
            Create => "create",
            Alias => "alias",
            Hidden => "hidden",
            Conversation => "conversation",
            Workspace => "workspace",
            Providers => "providers",
            ProviderKey => "provider-key",
            ProviderJobStart => "provider-job-start",
            ProviderJob => "provider-job",
            ProviderJobInput => "provider-job-input",
            ProviderJobCancel => "provider-job-cancel",
            Unspecified => return Err(Error::Unsupported),
        },
    )
}
fn session(value: w::SessionIdentity) -> Option<p::Session> {
    if value == w::SessionIdentity::default() {
        return None;
    }
    Some(p::Session {
        id: value.id,
        created_at: value.created_at,
    })
}
fn old_session(value: Option<p::Session>) -> w::SessionIdentity {
    value
        .map(|s| w::SessionIdentity {
            id: s.id,
            created_at: s.created_at,
        })
        .unwrap_or_default()
}
fn payload(raw: Option<Box<RawValue>>) -> Bytes {
    raw.map(|r| Bytes::copy_from_slice(r.get().as_bytes()))
        .unwrap_or_default()
}
fn old_payload(raw: Bytes) -> Result<Option<Box<RawValue>>, Error> {
    if raw.is_empty() {
        return Ok(None);
    }
    let string = String::from_utf8(raw.to_vec()).map_err(|_| Error::Fields)?;
    RawValue::from_string(string)
        .map(Some)
        .map_err(|_| Error::Fields)
}
fn u32_field(value: i64) -> Result<u32, Error> {
    value.try_into().map_err(|_| Error::Fields)
}

pub fn from_json(message: w::Message, direction: Direction) -> Result<p::Envelope, Error> {
    from_json_with_context(message, direction, None)
}
pub fn from_json_with_context(
    message: w::Message,
    direction: Direction,
    context: Option<ResponseContext>,
) -> Result<p::Envelope, Error> {
    use p::envelope::Body::*;
    message.validate_fields().map_err(|_| Error::Fields)?;
    if message
        .payload
        .as_ref()
        .is_some_and(|raw| raw.get().len() > w::MAX_MESSAGE)
        || message
            .header
            .as_ref()
            .is_some_and(|h| h.files.as_ref().is_some_and(|files| files.len() > 16))
    {
        return Err(Error::Size);
    }
    let m = message;
    let body = match m.kind.as_str() {
        "hello" => Hello(p::Hello {
            capabilities: m.capabilities,
        }),
        "request" => {
            let op = operation(&m.operation)?;
            Request(Box::new(p::Request {
                id: m.id,
                operation: op as i32,
                session: session(m.session),
                payload: Some(actions::request_from_json(op, &payload(m.payload))?),
            }))
        }
        "response" | "exit" | "refresh-result" | "upload-complete" | "upload-error" => {
            let known = match m.kind.as_str() {
                "exit" | "refresh-result" | "upload-error" => Some(ResponseContext::Empty),
                "upload-complete" => Some(ResponseContext::Upload),
                _ => context,
            };
            let raw = payload(m.payload);
            let result = if !m.error.is_empty() {
                if !raw.is_empty() {
                    return Err(Error::Fields);
                }
                None
            } else {
                actions::response_from_json(&raw, known.ok_or(Error::Fields)?)?
            };
            let response = p::Response {
                id: m.id,
                error: m.error,
                result,
            };
            match m.kind.as_str() {
                "response" => Response(response),
                "exit" => TerminalExit(response),
                "refresh-result" => RefreshResult(response),
                "upload-complete" => UploadComplete(response),
                _ => UploadError(response),
            }
        }
        "open" => TerminalOpen(p::TerminalOpen {
            id: m.id,
            session: session(m.session),
            cols: m.cols.into(),
            rows: m.rows.into(),
            capabilities: m.capabilities,
        }),
        "input" | "data" | "upload-data" => {
            let data = p::Data {
                id: m.id,
                data: m.data.into(),
            };
            match m.kind.as_str() {
                "input" => TerminalInput(data),
                "data" => TerminalOutput(data),
                _ => UploadData(data),
            }
        }
        "resize" => Resize(p::Resize {
            id: m.id,
            cols: m.cols.into(),
            rows: m.rows.into(),
        }),
        "close" | "cancel" | "refresh" | "upload-finish" | "upload-cancel" | "upload-ready" => {
            let reference = p::Reference { id: m.id };
            match m.kind.as_str() {
                "close" => Close(reference),
                "cancel" => Cancel(reference),
                "refresh" => Refresh(reference),
                "upload-finish" => UploadFinish(reference),
                "upload-cancel" => UploadCancel(reference),
                _ => UploadReady(reference),
            }
        }
        "output-ack" | "upload-ack" => {
            let ack = p::Ack {
                id: m.id,
                received: m.received,
            };
            if m.kind == "output-ack" {
                OutputAck(ack)
            } else {
                UploadAck(ack)
            }
        }
        "catalog" => Catalog(Box::new(
            snapshots::catalog_from_json(&payload(m.payload)).map_err(|_| Error::Fields)?,
        )),
        "usage" => {
            let snapshot =
                hmux_usage::transport::decode(&payload(m.payload)).map_err(|_| Error::Fields)?;
            Usage(Box::new(
                snapshots::usage_to_proto(snapshot).map_err(|_| Error::Fields)?,
            ))
        }
        "usage-unavailable" => UsageUnavailable(p::Empty {}),
        "task-complete" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Completion {
                completed_at: String,
            }
            let completion: Completion =
                serde_json::from_slice(&payload(m.payload)).map_err(|_| Error::Fields)?;
            TaskComplete(p::Completion {
                id: m.id,
                session: session(m.session),
                completed_at: completion.completed_at,
            })
        }
        "upload-start" => {
            let h = m.header.ok_or(Error::Fields)?;
            let files = h
                .files
                .unwrap_or_default()
                .into_iter()
                .map(|f| {
                    Ok(p::FileHeader {
                        index: u32_field(f.index)?,
                        size: f.size,
                        extension: f.extension,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            UploadStart(p::UploadStart {
                id: m.id,
                header: Some(p::UploadHeader {
                    protocol_version: u32_field(h.protocol_version)?,
                    request_id: h.request_id,
                    session: session(h.session),
                    file_count: u32_field(h.file_count)?,
                    total_bytes: h.total_bytes,
                    files,
                }),
            })
        }
        _ => return Err(Error::Unsupported),
    };
    let envelope = p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    };
    pb::validate(&envelope, direction)?;
    Ok(envelope)
}

pub fn to_json(envelope: p::Envelope, direction: Direction) -> Result<w::Message, Error> {
    use p::envelope::Body::*;
    pb::validate(&envelope, direction)?;
    let mut m = w::Message::default();
    let body = envelope.body.ok_or(Error::Fields)?;
    m.kind = match &body {
        Hello(_) => "hello",
        Request(_) => "request",
        Response(_) => "response",
        TerminalOpen(_) => "open",
        TerminalInput(_) => "input",
        TerminalOutput(_) => "data",
        Resize(_) => "resize",
        Close(_) => "close",
        Cancel(_) => "cancel",
        OutputAck(_) => "output-ack",
        TerminalExit(_) => "exit",
        Refresh(_) => "refresh",
        RefreshResult(_) => "refresh-result",
        Catalog(_) => "catalog",
        Usage(_) => "usage",
        UsageUnavailable(_) => "usage-unavailable",
        TaskComplete(_) => "task-complete",
        UploadStart(_) => "upload-start",
        UploadData(_) => "upload-data",
        UploadFinish(_) => "upload-finish",
        UploadCancel(_) => "upload-cancel",
        UploadReady(_) => "upload-ready",
        UploadAck(_) => "upload-ack",
        UploadComplete(_) => "upload-complete",
        UploadError(_) => "upload-error",
    }
    .into();
    match body {
        Hello(v) => m.capabilities = v.capabilities,
        Request(v) => {
            m.payload = old_payload(actions::request_payload(&v)?)?;
            m.id = v.id;
            m.operation = operation_name(v.operation)?.into();
            m.session = old_session(v.session);
        }
        Response(v) | TerminalExit(v) | RefreshResult(v) | UploadComplete(v) | UploadError(v) => {
            m.payload = old_payload(actions::response_payload(&v)?)?;
            m.id = v.id;
            m.error = v.error;
        }
        TerminalOpen(v) => {
            m.id = v.id;
            m.session = old_session(v.session);
            m.cols = v.cols as u16;
            m.rows = v.rows as u16;
            m.capabilities = v.capabilities;
        }
        TerminalInput(v) | TerminalOutput(v) | UploadData(v) => {
            m.id = v.id;
            m.data = v.data.to_vec();
        }
        Resize(v) => {
            m.id = v.id;
            m.cols = v.cols as u16;
            m.rows = v.rows as u16;
        }
        Close(v) | Cancel(v) | Refresh(v) | UploadFinish(v) | UploadCancel(v) | UploadReady(v) => {
            m.id = v.id
        }
        OutputAck(v) | UploadAck(v) => {
            m.id = v.id;
            m.received = v.received;
        }
        Catalog(v) => m.payload = old_payload(catalog_payload(*v)?)?,
        Usage(v) => m.payload = old_payload(usage_payload(*v)?)?,
        UsageUnavailable(_) => {}
        TaskComplete(v) => {
            m.id = v.id;
            m.session = old_session(v.session);
            m.payload = Some(
                RawValue::from_string(
                    serde_json::json!({"completed_at":v.completed_at}).to_string(),
                )
                .map_err(|_| Error::Fields)?,
            );
        }
        UploadStart(v) => {
            m.id = v.id;
            let h = v.header.ok_or(Error::Fields)?;
            m.header = Some(w::UploadHeader {
                protocol_version: h.protocol_version.into(),
                request_id: h.request_id,
                session: old_session(h.session),
                file_count: h.file_count.into(),
                total_bytes: h.total_bytes,
                files: Some(
                    h.files
                        .into_iter()
                        .map(|f| w::FileHeader {
                            index: f.index.into(),
                            size: f.size,
                            extension: f.extension,
                        })
                        .collect(),
                ),
            });
        }
    }
    Ok(m)
}

/// JSON is a presentation/compatibility boundary, never a typed snapshot wire body.
/// Serialize under the same byte cap before the HTTP cache or v1 frame retains it.
pub fn catalog_payload(value: p::CatalogSnapshot) -> Result<Bytes, Error> {
    let catalog = snapshots::catalog_from_proto(value).map_err(|_| Error::Fields)?;
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > snapshots::MAX_CATALOG_BYTES - self.0.len() {
                return Err(std::io::Error::other("catalog limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut raw = Bounded(Vec::new());
    serde_json::to_writer(&mut raw, &catalog).map_err(|_| Error::Size)?;
    Ok(Bytes::from(raw.0))
}
pub fn usage_payload(value: p::UsageSnapshot) -> Result<Bytes, Error> {
    let snapshot = snapshots::usage_from_proto(value).map_err(|_| Error::Fields)?;
    hmux_usage::transport::encode(&snapshot)
        .map(Bytes::from)
        .map_err(|_| Error::Size)
}
