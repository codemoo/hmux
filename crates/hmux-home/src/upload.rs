//! Bounded upload ownership. The WebSocket reader never performs spool I/O.
//! A single blocking worker owns each stage, its flock and its cleanup. The
//! async owner joins that worker before returning its process-wide admission.
use crate::{filestage::Store, peer, view};
use bytes::Bytes;
use hmux_core::command::CommandRunner;
use hmux_model::SessionIdentity;
use hmux_protocol::{
    protobuf::{types as p, Negotiated},
    transport::Sender,
    wire,
};
use std::{
    sync::{mpsc as sync_channel, Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub(crate) const CAPABILITY: &str = "web-upload-v1";
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static VERIFY_RUNNER: OnceLock<CommandRunner> = OnceLock::new();
static SWEEP_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(crate) enum Input {
    Data(Bytes),
    Finish,
}
pub(crate) struct Handle {
    pub stop: CancellationToken,
    input: mpsc::Sender<Input>,
}
impl Handle {
    pub fn input(&self, input: Input) {
        if self.stop.is_cancelled() {
            return;
        }
        // A decoded Bytes slice may retain the much larger wire frame. Copy
        // only the admitted chunk after obtaining its single queue reservation.
        let Ok(reserved) = self.input.try_reserve() else {
            self.stop.cancel();
            return;
        };
        let input = match input {
            Input::Data(data) if !data.is_empty() && data.len() <= wire::MAX_UPLOAD_CHUNK => {
                Input::Data(Bytes::copy_from_slice(&data))
            }
            Input::Finish => Input::Finish,
            _ => {
                self.stop.cancel();
                return;
            }
        };
        reserved.send(input);
    }
}
pub(crate) fn admit(
    link: &CancellationToken,
) -> Option<(Handle, mpsc::Receiver<Input>, OwnedSemaphorePermit)> {
    let permit = SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .ok()?;
    let (input, receiver) = mpsc::channel(1);
    Some((
        Handle {
            input,
            stop: link.child_token(),
        },
        receiver,
        permit,
    ))
}

fn now() -> Result<i64, ()> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ())?
        .as_secs()
        .try_into()
        .map_err(|_| ())
}

enum Work {
    Write(Bytes, oneshot::Sender<Result<i64, ()>>),
    Commit(oneshot::Sender<Result<p::StageResult, ()>>),
    Accept,
}

pub(crate) struct Job {
    pub header: p::UploadHeader,
    pub store: Arc<Store>,
    pub target: view::Target,
    pub sender: Sender,
    pub protocol: Negotiated,
    pub link_stop: CancellationToken,
}
impl Job {
    async fn verify(
        &self,
        session: &SessionIdentity,
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), ()> {
        let runner = VERIFY_RUNNER
            .get_or_init(|| CommandRunner::new(2).expect("nonzero upload verification limit"));
        view::verify(
            &self.target,
            runner,
            session,
            stop,
            deadline.min(Instant::now() + VERIFY_TIMEOUT),
        )
        .await
        .map_err(|_| ())
    }

    pub async fn run(
        self,
        handle: &Handle,
        mut input: mpsc::Receiver<Input>,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), peer::Error> {
        let stop = handle.stop.clone();
        let _cancel_on_return = stop.clone().drop_guard();
        let deadline = Instant::now() + UPLOAD_TIMEOUT;
        let id = self.header.request_id.clone();
        let Some(session) = self.header.session.as_ref().map(|s| SessionIdentity {
            id: s.id.clone(),
            created_at: s.created_at,
        }) else {
            return Ok(());
        }; // Already rejected by the wire decoder.
        if self.verify(&session, &stop, deadline).await.is_err() {
            return self.fail(&id).await;
        }

        let (work, commands) = sync_channel::sync_channel::<Work>(1);
        let (ready, initialized) = oneshot::channel();
        let store = self.store.clone();
        let header = self.header.clone();
        let worker_stop = stop.clone();
        // Keep a separate permit reference in the worker, including stage Drop,
        // so an OS call that stalls cannot admit unlimited replacement workers.
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let stage = now().and_then(|time| {
                store
                    .begin(header, time, worker_stop, deadline.into_std())
                    .map_err(|_| ())
            });
            let mut stage = match stage {
                Ok(stage) => stage,
                Err(()) => {
                    let _ = ready.send(Err(()));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                return;
            }
            while let Ok(command) = commands.recv() {
                match command {
                    Work::Write(data, reply) => {
                        let result = stage.write(&data).map_err(|_| ());
                        let failed = result.is_err();
                        if reply.send(result).is_err() || failed {
                            return;
                        }
                    }
                    Work::Commit(reply) => {
                        let result =
                            now().and_then(|time| stage.commit_typed(time).map_err(|_| ()));
                        let failed = result.is_err();
                        if reply.send(result).is_err() || failed {
                            return;
                        }
                    }
                    Work::Accept => {
                        stage.accept();
                        return;
                    }
                }
            }
            // Disconnecting the command sender wakes this worker immediately;
            // Stage Drop removes only its owned stage while retaining the lock.
        });

        let result = async {
            wait(initialized, &stop, deadline).await?;
            emit(&self.sender, self.protocol, p::envelope::Body::UploadReady(p::Reference { id: id.clone() }), &stop, deadline).await?;
            loop {
                let command = tokio::select! {
                    biased;
                    _ = stop.cancelled() => return Err(()),
                    _ = tokio::time::sleep_until(deadline.min(Instant::now() + IDLE_TIMEOUT)) => return Err(()),
                    command = input.recv() => command.ok_or(())?,
                };
                match command {
                    Input::Data(data) => {
                        let (reply, result) = oneshot::channel();
                        work.try_send(Work::Write(data, reply)).map_err(|_| ())?;
                        let received = wait(result, &stop, deadline).await?;
                        emit(&self.sender, self.protocol, p::envelope::Body::UploadAck(p::Ack { id: id.clone(), received }), &stop, deadline).await?;
                    }
                    Input::Finish => {
                        self.verify(&session, &stop, deadline).await?;
                        let (reply, result) = oneshot::channel();
                        work.try_send(Work::Commit(reply)).map_err(|_| ())?;
                        let staged = wait(result, &stop, deadline).await?;
                        emit(&self.sender, self.protocol, p::envelope::Body::UploadComplete(p::Response { id: id.clone(), result: Some(p::response::Result::Staged(Box::new(staged))), error: String::new() }), &stop, deadline).await?;
                        // Wire delivery is the ownership handoff. A subsequent
                        // cancel must not delete files already promised to the UI.
                        work.try_send(Work::Accept).map_err(|_| ())?;
                        return Ok(());
                    }
                }
            }
        }.await;
        stop.cancel();
        drop(work);
        drop(input);
        let joined = worker.await;
        if result.is_err() || joined.is_err() {
            self.fail(&id).await
        } else {
            Ok(())
        }
    }

    async fn fail(&self, id: &str) -> Result<(), peer::Error> {
        if self.link_stop.is_cancelled() {
            return Ok(());
        }
        peer::send(
            &self.sender,
            self.protocol,
            error(id.to_owned()),
            self.link_stop.clone(),
        )
        .await
    }
}

pub(crate) fn error(id: String) -> p::envelope::Body {
    p::envelope::Body::UploadError(p::Response {
        id,
        result: None,
        error: "Home upload unavailable".into(),
    })
}
async fn wait<T>(
    receiver: oneshot::Receiver<Result<T, ()>>,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<T, ()> {
    tokio::select! {
        biased;
        _ = stop.cancelled() => Err(()),
        _ = tokio::time::sleep_until(deadline.min(Instant::now() + IDLE_TIMEOUT)) => Err(()),
        result = receiver => result.map_err(|_| ())?,
    }
}
async fn emit(
    sender: &Sender,
    protocol: Negotiated,
    body: p::envelope::Body,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<(), ()> {
    if stop.is_cancelled() || Instant::now() >= deadline {
        return Err(());
    }
    // Once a frame starts, only the transport owns its bounded write deadline.
    // Await its delivery receipt even if cancelled meanwhile: a complete frame
    // may already be on the wire, and upload-complete must then retain files.
    peer::send(sender, protocol, body, stop.clone())
        .await
        .map_err(|_| ())
}

/// One joined sweep at startup and each minute, including while reconnecting.
/// Retain the process-wide permit through actual filesystem completion.
pub(crate) async fn sweep(store: Arc<Store>, stop: CancellationToken) {
    loop {
        let permit = tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            permit = SWEEP_SLOT.get_or_init(|| Arc::new(Semaphore::new(1))).clone().acquire_owned() => permit.expect("sweep admission never closed"),
        };
        let work_store = store.clone();
        let work_stop = stop.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Ok(time) = now() {
                let _ = work_store.sweep(time, work_stop, std::time::Instant::now() + IDLE_TIMEOUT);
            }
        })
        .await;
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {},
        }
    }
}
