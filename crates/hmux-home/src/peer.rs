//! Connected Home catalog, profiles and disposable terminal owner. The caller
//! owns WSS, authentication, reconnect and shared workspace/provider/recovery owners.
use crate::{
    catalog::TmuxCatalogReader,
    completion,
    config::{load_inventory, HomeConfig},
    conversation,
    filestage::Store,
    inspection::{self, Inspector},
    metrics, sessions, terminal, upload, usage,
};
use hmux_core::command::CommandRunner;
use hmux_protocol::{
    actions, legacy,
    protobuf::{self, types as p, Direction, Negotiated},
    snapshots,
    transport::{self, Incoming, Sender},
    wire,
};
use prost::Message as _;
use serde::{
    ser::{SerializeSeq, Serializer as _},
    Serialize,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet};
use tokio_util::sync::CancellationToken;

const CATALOG_INTERVAL: Duration = Duration::from_secs(5);
// Both legacy and Rust gateways require a recent full catalog (<40 seconds).
// No wire-level catalog lease exists yet, so periodically renew unchanged state.
const CATALOG_RENEWAL: Duration = Duration::from_secs(15);
const ACTION_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_SLOTS: usize = 8;
// Leave ample room for the v1 JSON wrapper or v2 envelope around opaque JSON.
const JSON_LIMIT: usize = wire::MAX_MESSAGE - 512;
const UNAVAILABLE: &str = "Home operation unavailable";
const BUSY: &str = "Home is busy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Config,
    Catalog,
    Encoding,
    Protocol,
    Transport,
    Worker,
}

#[cfg(test)]
struct Bounded(Vec<u8>);
#[cfg(test)]
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > JSON_LIMIT - self.0.len() {
            return Err(io::Error::other("JSON limit"));
        }
        let needed = self.0.len() + bytes.len();
        if needed > self.0.capacity() {
            let capacity = needed
                .max(self.0.capacity().max(4096).saturating_mul(2))
                .min(JSON_LIMIT);
            self.0
                .try_reserve_exact(capacity - self.0.len())
                .map_err(io::Error::other)?;
            if self.0.capacity() > JSON_LIMIT {
                return Err(io::Error::other("JSON allocation limit"));
            }
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|&n| n <= wire::MAX_MESSAGE)
            .ok_or_else(|| io::Error::other("frame limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) async fn send(
    sender: &Sender,
    protocol: Negotiated,
    body: p::envelope::Body,
    cancel: CancellationToken,
) -> Result<(), Error> {
    let envelope = p::Envelope {
        version: protobuf::VERSION,
        body: Some(body),
    };
    match protocol {
        Negotiated::ProtobufV2 => {
            let size = envelope.encoded_len();
            let reserved = sender.try_reserve(size).map_err(|_| Error::Transport)?;
            let raw =
                protobuf::encode(&envelope, Direction::ToGateway).map_err(|_| Error::Encoding)?;
            reserved
                .submit(&raw, cancel)
                .map_err(|_| Error::Transport)?
                .wait()
                .await
                .map_err(|_| Error::Transport)
        }
        Negotiated::JsonV1 => {
            let message =
                legacy::to_json(envelope, Direction::ToGateway).map_err(|_| Error::Encoding)?;
            let mut count = Count(0);
            serde_json::to_writer(&mut count, &message).map_err(|_| Error::Encoding)?;
            let reserved = sender.try_reserve(count.0).map_err(|_| Error::Transport)?;
            let raw = message.encode().map_err(|_| Error::Encoding)?;
            if raw.len() != count.0 {
                return Err(Error::Encoding);
            }
            reserved
                .submit(&raw, cancel)
                .map_err(|_| Error::Transport)?
                .wait()
                .await
                .map_err(|_| Error::Transport)
        }
    }
}

fn response(id: String, result: Option<p::response::Result>, error: &str) -> p::envelope::Body {
    p::envelope::Body::Response(p::Response {
        id,
        result,
        error: error.into(),
    })
}
fn provider_operation(operation: i32) -> Option<&'static str> {
    match p::Operation::try_from(operation).ok()? {
        p::Operation::Providers => Some("providers"),
        p::Operation::ProviderKey => Some("provider-key"),
        p::Operation::ProviderJobStart => Some("provider-job-start"),
        p::Operation::ProviderJob => Some("provider-job"),
        p::Operation::ProviderJobInput => Some("provider-job-input"),
        p::Operation::ProviderJobCancel => Some("provider-job-cancel"),
        _ => None,
    }
}

fn unsupported(id: String) -> p::envelope::Body {
    response(id, None, UNAVAILABLE)
}
fn recoverable_legacy_action(value: &wire::Message, duplicate: bool) -> bool {
    value.kind == "request"
        && !value.id.is_empty()
        && value.validate_fields().is_ok()
        && legacy::operation(&value.operation).is_ok()
        && !duplicate
}

fn profiles(path: PathBuf) -> Result<p::ProfilesResult, Error> {
    #[derive(Serialize)]
    struct PublicProfile<'a> {
        id: &'a str,
        label: &'a str,
    }
    struct ProfileCount(usize);
    impl Write for ProfileCount {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|&n| n <= JSON_LIMIT)
                .ok_or_else(|| io::Error::other("profiles exceed response limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let inventory = load_inventory(&path).map_err(|_| Error::Config)?;
    let values = inventory.profiles.as_deref().ok_or(Error::Config)?;
    let mut count = ProfileCount(0);
    {
        let mut serializer = serde_json::Serializer::new(&mut count);
        let mut seq = serializer
            .serialize_seq(Some(values.len()))
            .map_err(|_| Error::Encoding)?;
        for value in values {
            seq.serialize_element(&PublicProfile {
                id: &value.id,
                label: &value.label,
            })
            .map_err(|_| Error::Encoding)?;
        }
        seq.end().map_err(|_| Error::Encoding)?;
    }
    Ok(p::ProfilesResult {
        items: values
            .iter()
            .map(|v| p::Profile {
                id: v.id.clone(),
                label: v.label.clone(),
            })
            .collect(),
    })
}

struct Enrichment {
    metadata: Option<PathBuf>,
    recovery: Option<crate::recovery::Store>,
    inspector: Option<Arc<Inspector>>,
    completion: Option<Arc<completion::Inbox>>,
    metrics: tokio::sync::watch::Receiver<Option<hmux_model::HostMetrics>>,
}
struct CatalogHash {
    hash: Sha256,
    bytes: usize,
}
impl Write for CatalogHash {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > JSON_LIMIT.saturating_sub(self.bytes) {
            return Err(io::Error::other("catalog exceeds limit"));
        }
        self.bytes += data.len();
        self.hash.update(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn catalog_digest(catalog: &mut hmux_model::Catalog) -> Result<[u8; 32], Error> {
    // Hash directly into a bounded sink; retain neither a second catalog nor
    // its encoded bytes on unchanged polls. Only generation time is excluded.
    let generated_at = std::mem::take(&mut catalog.generated_at);
    let mut sink = CatalogHash {
        hash: Sha256::new(),
        bytes: 0,
    };
    let result = serde_json::to_writer(&mut sink, &catalog);
    catalog.generated_at = generated_at;
    result.map_err(|_| Error::Encoding)?;
    Ok(sink.hash.finalize().into())
}
async fn publish_snapshot(
    mut catalog: hmux_model::Catalog,
    previous: &mut Option<([u8; 32], tokio::time::Instant)>,
    sender: &Sender,
    protocol: Negotiated,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let digest = catalog_digest(&mut catalog)?;
    if previous
        .as_ref()
        .is_some_and(|(old, at)| *old == digest && at.elapsed() < CATALOG_RENEWAL)
    {
        return Ok(());
    }
    let catalog = snapshots::catalog_to_proto(catalog).map_err(|_| Error::Encoding)?;
    send(
        sender,
        protocol,
        p::envelope::Body::Catalog(Box::new(catalog)),
        stop.clone(),
    )
    .await?;
    *previous = Some((digest, tokio::time::Instant::now()));
    Ok(())
}
async fn publish_catalog(
    reader: TmuxCatalogReader,
    runner: CommandRunner,
    sender: Sender,
    protocol: Negotiated,
    stop: CancellationToken,
    enrichment: Enrichment,
    changed: Arc<tokio::sync::Notify>,
) -> Result<(), Error> {
    let Enrichment {
        metadata,
        recovery,
        inspector,
        completion,
        metrics,
    } = enrichment;
    let mut first = true;
    let mut previous = None;
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        // Do not drop an in-flight command future on cancellation: CommandRunner
        // owns and reaps the direct child before it returns.
        let catalog = reader
            .read_basic_cancelable(
                &runner,
                &stop,
                tokio::time::Instant::now() + Duration::from_secs(10),
            )
            .await;
        if stop.is_cancelled() {
            return Ok(());
        }
        let mut catalog = catalog.map_err(|_| Error::Catalog)?;
        catalog.host_metrics = metrics.borrow().clone();
        if let Some(inbox) = &completion {
            inbox.enqueue(&catalog);
        }
        let catalog = if let Some(path) = &metadata {
            sessions::overlay(catalog, path.clone(), recovery.clone(), &stop)
                .await
                .map_err(|_| Error::Catalog)?
        } else {
            catalog
        };
        if first && inspector.is_some() && catalog.sessions.iter().flatten().any(|s| s.pane_pid > 0)
        {
            // Publish first readiness before process or transcript metadata I/O.
            publish_snapshot(catalog.clone(), &mut previous, &sender, protocol, &stop).await?;
        }
        let catalog = if let Some(inspector) = &inspector {
            inspector
                .clone()
                .annotate(catalog, &stop)
                .await
                .map_err(|_| Error::Worker)?
        } else {
            catalog
        };
        if stop.is_cancelled() {
            return Ok(());
        }
        publish_snapshot(catalog, &mut previous, &sender, protocol, &stop).await?;
        first = false;
        tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            _ = tokio::time::sleep(CATALOG_INTERVAL) => {},
            _ = changed.notified() => {},
        }
    }
}

async fn serve_profiles(
    id: String,
    path: PathBuf,
    sender: Sender,
    protocol: Negotiated,
    stop: CancellationToken,
    cancelled: CancellationToken,
    permits: (
        tokio::sync::OwnedSemaphorePermit,
        tokio::sync::OwnedSemaphorePermit,
    ),
) -> (String, Result<(), Error>) {
    let (slot, io_slot) = permits;
    // The blocking worker owns its permit through real completion, even if a
    // request is cancelled or its response deadline expires.
    let mut worker = tokio::task::spawn_blocking(move || {
        let _io_slot = io_slot;
        profiles(path)
    });
    let mut finished = false;
    let result = tokio::select! {
        biased;
        _ = stop.cancelled() => None,
        _ = cancelled.cancelled() => Some(Err(Error::Config)),
        _ = tokio::time::sleep(ACTION_TIMEOUT) => Some(Err(Error::Config)),
        value = &mut worker => { finished = true; Some(value.unwrap_or(Err(Error::Worker))) },
    };
    let sent = if let Some(result) = result {
        let body = if cancelled.is_cancelled() || stop.is_cancelled() {
            unsupported(id.clone())
        } else {
            match result {
                Ok(payload) => {
                    response(id.clone(), Some(p::response::Result::Profiles(payload)), "")
                }
                Err(_) => unsupported(id.clone()),
            }
        };
        if stop.is_cancelled() {
            Ok(())
        } else {
            send(&sender, protocol, body, stop.clone()).await
        }
    } else {
        Ok(())
    };
    if !finished {
        let _ = worker.await;
    }
    drop(slot);
    (id, sent)
}

/// Consume one authenticated connection configured for incoming `ToHome` frames.
/// WSS, reconnect and the lifetime connector lock belong to its caller. Returning
/// joins this peer's catalog, inventory, terminal and transport owners. Dropping this future
/// requests cancellation; one private peer task retains and joins those owners.
/// For completed cleanup, cancel `shutdown` and await this future rather than
/// aborting it. Keep Tokio alive through cleanup and use a finite process policy
/// for OS operations that cannot be interrupted.
pub async fn run_connected(
    connection: transport::Connection,
    config: HomeConfig,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    run_connected_with_uploads(connection, config, catalog, runner, None, shutdown).await
}

/// Explicit staging avoids opening a user's cache from library fixtures. The
/// connector owns the store's startup/periodic sweeper across reconnects.
pub async fn run_connected_with_uploads(
    connection: transport::Connection,
    config: HomeConfig,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
    store: Option<Arc<Store>>,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    run_connected_with_services(
        connection,
        config,
        catalog,
        runner,
        Services {
            uploads: store,
            sessions: None,
            ..Services::default()
        },
        shutdown,
    )
    .await
}

#[derive(Default)]
pub struct Services {
    pub usage: Option<usage::Receiver>,
    pub usage_refresh: Option<usage::RefreshHandle>,
    pub providers: Option<Arc<crate::providers::ProviderService>>,
    pub recovery: Option<crate::recovery::Store>,
    pub workspace: Option<crate::workspace::Workspace>,
    pub completions: bool,
    pub metrics: Option<Arc<metrics::Collector>>,
    pub inspector: Option<Arc<Inspector>>,
    pub uploads: Option<Arc<Store>>,
    pub sessions: Option<Arc<sessions::Context>>,
}

pub async fn run_connected_with_services(
    connection: transport::Connection,
    config: HomeConfig,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
    services: Services,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let stop = shutdown.child_token();
    let _cancel_on_drop = stop.clone().drop_guard();
    tokio::spawn(run_owned(
        connection, config, catalog, runner, services, stop,
    ))
    .await
    .map_err(|_| Error::Worker)?
}

async fn run_owned(
    connection: transport::Connection,
    config: HomeConfig,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
    services: Services,
    stop: CancellationToken,
) -> Result<(), Error> {
    let Services {
        uploads: store,
        sessions: session_context,
        inspector,
        completions,
        metrics,
        usage,
        usage_refresh,
        providers,
        recovery,
        workspace,
    } = services;
    let _cancel_on_unwind = stop.clone().drop_guard();
    let protocol = connection.protocol();
    let transport::Connection {
        sender,
        mut reader,
        task,
    } = connection;
    let target = catalog.terminal_target();
    let mut result = Ok(());
    if config.role != "home" || target.is_err() {
        result = Err(Error::Config);
    }
    if result.is_ok() {
        let mut capabilities = vec![hmux_protocol::flow::CAPABILITY.into()];
        if store.is_some() {
            capabilities.push(upload::CAPABILITY.into());
        }
        result = tokio::select! {
            _ = stop.cancelled() => Ok(()),
            value = send(&sender, protocol, p::envelope::Body::Hello(p::Hello { capabilities }), stop.clone()) => value,
        };
    }
    if result.is_err() || stop.is_cancelled() {
        sender.close();
        drop(reader);
        task.await.map_err(|_| Error::Worker)?;
        return result;
    }
    let usage_worker = usage.map(|latest| {
        tokio::spawn(usage::forward(
            latest,
            sender.clone(),
            protocol,
            stop.clone(),
        ))
    });
    let target = target.expect("validated terminal target");
    let changed = Arc::new(tokio::sync::Notify::new());
    let completion_inbox =
        (completions && inspector.is_some()).then(|| Arc::new(completion::Inbox::default()));
    let completion_worker = completion_inbox.as_ref().map(|inbox| {
        tokio::spawn(completion::run(
            inbox.clone(),
            inspector.as_ref().expect("configured").clone(),
            sender.clone(),
            protocol,
            stop.clone(),
        ))
    });
    let (metrics_sender, metrics_latest) = tokio::sync::watch::channel(None);
    let metrics_worker =
        metrics.map(|source| tokio::spawn(metrics::run(source, metrics_sender, stop.clone())));
    let mut collector = tokio::spawn(publish_catalog(
        catalog.clone(),
        runner.clone(),
        sender.clone(),
        protocol,
        stop.clone(),
        Enrichment {
            metadata: session_context.as_ref().map(|_| config.state_dir.clone()),
            recovery,
            inspector: inspector.clone(),
            completion: completion_inbox,
            metrics: metrics_latest,
        },
        changed.clone(),
    ));
    let slots = Arc::new(Semaphore::new(REQUEST_SLOTS));
    let profile_slot = Arc::new(Semaphore::new(1));
    let mut jobs = JoinSet::new();
    let mut pending = HashMap::<String, CancellationToken>::new();
    let mut terminals = HashMap::<String, Arc<terminal::Handle>>::new();
    let mut uploads = HashMap::<String, Arc<upload::Handle>>::new();
    let mut collector_done = false;
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            outcome = &mut collector => {
                collector_done = true;
                result = outcome.map_err(|_| Error::Worker).and_then(|v| v);
                break;
            },
            Some(done) = jobs.join_next(), if !jobs.is_empty() => {
                match done {
                    Ok((id, Ok(()))) => { pending.remove(&id); terminals.remove(&id); uploads.remove(&id); },
                    Ok((id, Err(error))) => { pending.remove(&id); terminals.remove(&id); uploads.remove(&id); result = Err(error); break; },
                    Err(_) => { result = Err(Error::Worker); break; },
                }
            },
            incoming = reader.receive() => {
                let envelope = match incoming {
                    Ok(Incoming::Protobuf(value)) if protocol == Negotiated::ProtobufV2 => value,
                    Ok(Incoming::Json(value)) if protocol == Negotiated::JsonV1 => {
                        let recoverable = recoverable_legacy_action(&value, pending.contains_key(&value.id));
                        let request_id = value.id.clone();
                        match legacy::from_json(value, Direction::ToHome) {
                            Ok(value) => value,
                            Err(hmux_protocol::protobuf::Error::Fields | hmux_protocol::protobuf::Error::Size) if recoverable => {
                                result = send(&sender, protocol, unsupported(request_id), stop.clone()).await;
                                if result.is_err() { break; }
                                continue;
                            }
                            Err(_) => { result = Err(Error::Protocol); break; }
                        }
                    }
                    Err(transport::Error::Protocol) => { result = Err(Error::Protocol); break; }
                    Err(_) => { result = Err(Error::Transport); break; }
                    _ => { result = Err(Error::Protocol); break; }
                };
                let Some(body) = envelope.body else { result = Err(Error::Protocol); break; };
                use p::envelope::Body::*;
                match body {
                    Request(request) => {
                        if pending.contains_key(&request.id) { result = Err(Error::Protocol); break; }
                        let permit = slots.clone().try_acquire_owned();
                        if permit.is_err() {
                            result = send(&sender, protocol, response(request.id, None, BUSY), stop.clone()).await;
                            if result.is_err() { break; }
                            continue;
                        }
                        let permit = permit.expect("checked");
                        if p::Operation::try_from(request.operation) == Ok(p::Operation::Profiles) {
                            let Ok(io_permit) = profile_slot.clone().try_acquire_owned() else {
                                result = send(&sender, protocol, response(request.id, None, BUSY), stop.clone()).await;
                                if result.is_err() { break; }
                                continue;
                            };
                            let id = request.id;
                            let cancellation = stop.child_token();
                            pending.insert(id.clone(), cancellation.clone());
                            jobs.spawn(serve_profiles(id, config.inventory_path.clone(), sender.clone(), protocol, stop.clone(), cancellation, (permit, io_permit)));
                        } else if provider_operation(request.operation).is_some() && providers.is_some() {
                            let operation = p::Operation::try_from(request.operation).expect("checked operation");
                            let Some(payload) = request.payload else { result = Err(Error::Protocol); break; };
                            let id = request.id;
                            let cancellation = stop.child_token();
                            pending.insert(id.clone(), cancellation.clone());
                            let providers = providers.as_ref().expect("configured").clone();
                            let refresh = usage_refresh.clone();
                            let sender = sender.clone();
                            let stop = stop.clone();
                            jobs.spawn(async move {
                                let outcome = providers.action_typed(operation, payload, cancellation.clone()).await;
                                let body = match outcome {
                                    Ok(result) => {
                                        if result.refresh_auth { if let Some(refresh) = refresh {refresh.request();} }
                                        if cancellation.is_cancelled() { unsupported(id.clone()) } else { response(id.clone(), Some(p::response::Result::Providers(Box::new(result.result))), "") }
                                    },
                                    _ => unsupported(id.clone()),
                                };
                                let sent = if stop.is_cancelled() { Ok(()) } else { send(&sender, protocol, body, stop).await };
                                drop(permit);
                                (id, sent)
                            });
                        } else if request.operation == p::Operation::Workspace as i32 && workspace.is_some() {
                            let Some(p::request::Payload::Workspace(payload)) = request.payload else { result = Err(Error::Protocol); break; };
                            let change = match payload.change.map(actions::workspace_change_from_proto).transpose() { Ok(v) => v, Err(_) => {result=Err(Error::Protocol);break;} };
                            let id = request.id;
                            let cancellation = stop.child_token();
                            pending.insert(id.clone(), cancellation.clone());
                            let workspace = workspace.as_ref().expect("configured").clone();
                            let catalog = catalog.clone();
                            let runner = runner.clone();
                            let sender = sender.clone();
                            let stop = stop.clone();
                            jobs.spawn(async move {
                                let outcome = workspace.request_typed(change, catalog, runner, &cancellation).await;
                                let body = match outcome {
                                    Ok(value) if !cancellation.is_cancelled() => response(id.clone(), Some(p::response::Result::Workspace(Box::new(actions::workspace_to_proto(value)))), ""),
                                    _ => unsupported(id.clone()),
                                };
                                let sent = if stop.is_cancelled() { Ok(()) } else { send(&sender, protocol, body, stop).await };
                                drop(permit);
                                (id, sent)
                            });
                        } else if request.operation == p::Operation::Conversation as i32 && inspector.is_some() {
                            let Some(session)=request.session else { result=Err(Error::Protocol);break; };
                            let Some(io_permit)=inspection::admit() else {
                                result=send(&sender,protocol,response(request.id,None,BUSY),stop.clone()).await;
                                if result.is_err(){break;} continue;
                            };
                            let id=request.id;
                            let cancellation=stop.child_token();
                            pending.insert(id.clone(),cancellation.clone());
                            let job=conversation::Job {inspector:inspector.as_ref().expect("configured").clone(),reader:catalog.clone(),identity:hmux_model::SessionIdentity{id:session.id,created_at:session.created_at},stop:cancellation.clone()};
                            let sender=sender.clone();let stop=stop.clone();
                            jobs.spawn(async move {
                                let outcome=job.run(io_permit).await;
                                let body=match outcome {Ok(value) if !cancellation.is_cancelled()=>response(id.clone(),Some(p::response::Result::Conversation(Box::new(actions::conversation_to_proto(value)))),""),_=>unsupported(id.clone())};
                                let sent=if stop.is_cancelled(){Ok(())}else{send(&sender,protocol,body,stop).await};
                                drop(permit);(id,sent)
                            });
                        } else if sessions::supported(request.operation) && session_context.is_some() {
                            let Some(action_permit) = sessions::admit() else {
                                result = send(&sender, protocol, response(request.id, None, BUSY), stop.clone()).await;
                                if result.is_err() { break; }
                                continue;
                            };
                            let id = request.id.clone();
                            let cancellation = stop.child_token();
                            pending.insert(id.clone(), cancellation.clone());
                            let job = sessions::Job { context: session_context.as_ref().expect("checked context").clone(), config: config.clone(), target: target.clone(), catalog: catalog.clone(), request: *request, stop: cancellation.clone() };
                            let sender = sender.clone();
                            let stop = stop.clone();
                            let changed = changed.clone();
                            jobs.spawn(async move {
                                let outcome = job.run(action_permit).await;
                                changed.notify_one();
                                let body = match outcome {
                                    Ok(value) if !cancellation.is_cancelled() => response(id.clone(), Some(value), ""),
                                    Err(sessions::Error::CreatedUnrecorded) => response(id.clone(), None, "Session created; metadata unavailable"),
                                    _ => unsupported(id.clone()),
                                };
                                let sent = if stop.is_cancelled() { Ok(()) } else { send(&sender, protocol, body, stop).await };
                                drop(permit);
                                (id, sent)
                            });
                        } else {
                            result = send(&sender, protocol, unsupported(request.id), stop.clone()).await;
                            if result.is_err() { break; }
                        }
                    }
                    TerminalOpen(open) => {
                        if pending.contains_key(&open.id) { result = Err(Error::Protocol); break; }
                        let permit = slots.clone().try_acquire_owned();
                        if permit.is_err() || terminals.len() >= wire::MAX_TERMINALS {
                            result = send(&sender, protocol, response(open.id, None, BUSY), stop.clone()).await;
                            if result.is_err() { break; }
                            continue;
                        }
                        let (handle, receiver) = terminal::channel(open.capabilities.iter().any(|cap| cap == hmux_protocol::flow::CAPABILITY), &stop);
                        let handle = Arc::new(handle);
                        let id = open.id.clone();
                        pending.insert(id.clone(), handle.stop.clone());
                        terminals.insert(id.clone(), handle.clone());
                        let job = terminal::Job { request:open, target:target.clone(), sender:sender.clone(), protocol, link_stop:stop.clone(), permit:permit.expect("checked") };
                        jobs.spawn(async move { let result=job.run(&handle,receiver).await; (id,result) });
                    }
                    UploadStart(start) => {
                        if pending.contains_key(&start.id) { result = Err(Error::Protocol); break; }
                        let admitted = store.as_ref().and_then(|store| upload::admit(&stop).map(|admission| (store.clone(), admission)));
                        let Some((store, (handle, receiver, permit))) = admitted else {
                            result = send(&sender, protocol, upload::error(start.id), stop.clone()).await;
                            if result.is_err() { break; }
                            continue;
                        };
                        let handle = Arc::new(handle);
                        let id = start.id;
                        let Some(header) = start.header else { result = Err(Error::Protocol); break; };
                        pending.insert(id.clone(), handle.stop.clone());
                        uploads.insert(id.clone(), handle.clone());
                        let job = upload::Job { header, store, target: target.clone(), sender: sender.clone(), protocol, link_stop: stop.clone() };
                        jobs.spawn(async move { let result = job.run(&handle, receiver, permit).await; (id, result) });
                    }
                    UploadData(data) => {
                        if let Some(upload) = uploads.get(&data.id) { upload.input(upload::Input::Data(data.data)); }
                    }
                    UploadFinish(reference) => {
                        if let Some(upload) = uploads.get(&reference.id) { upload.input(upload::Input::Finish); }
                    }
                    TerminalInput(data) => {
                        if let Some(terminal)=terminals.get(&data.id) { terminal.input(terminal::Input::Data(data.data)); }
                    }
                    Resize(resize) => {
                        if let Some(terminal)=terminals.get(&resize.id) { terminal.input(terminal::Input::Resize(resize.cols as u16,resize.rows as u16)); }
                    }
                    Refresh(reference) => {
                        if let Some(terminal)=terminals.get(&reference.id) { terminal.input(terminal::Input::Refresh); }
                    }
                    Cancel(reference) | Close(reference) => {
                        if let Some(request) = pending.get(&reference.id) { request.cancel(); }
                    }
                    OutputAck(ack) => { if let Some(terminal)=terminals.get(&ack.id) { terminal.acknowledge(ack.received); } }
                    UploadCancel(reference) => { if let Some(upload) = uploads.get(&reference.id) { upload.stop.cancel(); } }
                    _ => { result = Err(Error::Protocol); break; }
                }
            }
        }
    }
    stop.cancel();
    sender.close();
    drop(reader);
    while jobs.join_next().await.is_some() {}
    if !collector_done {
        let _ = collector.await;
    }
    if let Some(worker) = completion_worker {
        let _ = worker.await;
    }
    if let Some(worker) = usage_worker {
        let _ = worker.await;
    }
    if let Some(worker) = metrics_worker {
        let _ = worker.await;
    }
    task.await.map_err(|_| Error::Worker)?;
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_legacy_action_can_reply_without_closing_peer() {
        let raw = br#"{"type":"request","id":"bad-create","operation":"create","payload":{"extra":true}}"#;
        let message = crate::peer::wire::Message::decode(raw).unwrap();
        assert!(super::recoverable_legacy_action(&message, false));
        assert!(
            crate::peer::legacy::from_json(message.clone(), crate::peer::Direction::ToHome)
                .is_err()
        );
        assert!(!super::recoverable_legacy_action(&message, true));
        let mut other = message;
        other.kind = "upload-data".into();
        assert!(!super::recoverable_legacy_action(&other, false));
    }

    use super::*;

    #[test]
    fn serialization_caps_backing_capacity_and_fits_both_envelopes() {
        // An uneven large first write previously made Vec's geometric growth
        // retain more backing capacity than the advertised logical JSON limit.
        let mut output = Bounded(Vec::new());
        output.write_all(b"\"").unwrap();
        output.write_all(&vec![b'x'; 3 << 20]).unwrap();
        let tail = JSON_LIMIT - output.0.len() - 1;
        output.write_all(&vec![b'x'; tail]).unwrap();
        output.write_all(b"\"").unwrap();
        assert_eq!(output.0.len(), JSON_LIMIT);
        assert!(output.0.capacity() <= JSON_LIMIT);
        let capacity = output.0.capacity();
        assert!(output.write_all(b"x").is_err());
        assert_eq!(output.0.capacity(), capacity);
        // Keep the catalog near the wire limit using a valid typed tree: each
        // session identity and bounded name is admitted by both codecs.
        let catalog = p::CatalogSnapshot {
            sessions: Some(p::CatalogSessions {
                items: (1..=900)
                    .map(|id| p::CatalogSession {
                        identity: Some(p::Session {
                            id: format!("${id}"),
                            created_at: id,
                        }),
                        name: "x".repeat(3900),
                        ..Default::default()
                    })
                    .collect(),
            }),
            ..Default::default()
        };
        for body in [
            p::envelope::Body::Catalog(Box::new(catalog)),
            response(
                "r".repeat(64),
                Some(p::response::Result::Created(p::CreatedResult {
                    id: "$1".into(),
                    created_at: 1,
                    reused: false,
                })),
                "",
            ),
        ] {
            let envelope = p::Envelope {
                version: protobuf::VERSION,
                body: Some(body),
            };
            let raw = protobuf::encode(&envelope, Direction::ToGateway).unwrap();
            assert!(raw.len() <= wire::MAX_MESSAGE);
            let message = legacy::to_json(envelope, Direction::ToGateway).unwrap();
            let mut count = Count(0);
            serde_json::to_writer(&mut count, &message).unwrap();
            assert_eq!(message.encode().unwrap().len(), count.0);
            assert!(count.0 <= wire::MAX_MESSAGE);
        }
    }
}
