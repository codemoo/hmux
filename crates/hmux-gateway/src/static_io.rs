//! Demand-driven local file reads. No prefetcher or per-client persistent task.
use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use std::{
    collections::VecDeque,
    fs::File,
    future::Future,
    io,
    os::unix::fs::FileExt,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub(super) const CHUNK_BYTES: usize = 32 << 10;
pub(super) const STREAMS: usize = 16;
const WORKERS: usize = 4;
pub(super) const RETAINED_CHUNKS: usize = 64;

pub(super) struct Resources {
    pub streams: Arc<Semaphore>,
    pub chunks: Arc<Semaphore>,
    workers: Arc<Semaphore>,
    stopped: CancellationToken,
}
impl Resources {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            streams: Arc::new(Semaphore::new(STREAMS)),
            chunks: Arc::new(Semaphore::new(RETAINED_CHUNKS)),
            workers: Arc::new(Semaphore::new(WORKERS)),
            stopped: CancellationToken::new(),
        })
    }
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        // The actual blocking worker owns admission after caller cancellation.
        let permit = tokio::select! { biased;
            _ = self.stopped.cancelled() => return Err(stopped()),
            permit = self.workers.clone().acquire_owned() => permit.map_err(|_| stopped())?,
        };
        if self.stopped.is_cancelled() {
            return Err(stopped());
        }
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|_| io::Error::other("asset worker failed"))?
    }
    pub async fn shutdown(&self) {
        self.stopped.cancel();
        // A racing caller holding a slot either finishes its worker or sees stop
        // before spawning. Do not abandon detached blocking reads on shutdown.
        let _drained = self
            .workers
            .clone()
            .acquire_many_owned(WORKERS as u32)
            .await;
        self.workers.close();
    }
}
fn stopped() -> io::Error {
    io::Error::other("asset service stopped")
}

pub(super) struct FileLease {
    pub file: File,
    pub _permit: OwnedSemaphorePermit,
}
pub(super) enum Part {
    Literal(Bytes),
    Span { start: u64, len: u64 },
}
impl Part {
    fn len(&self) -> u64 {
        match self {
            Self::Literal(bytes) => bytes.len() as u64,
            Self::Span { len, .. } => *len,
        }
    }
}
struct Chunk {
    data: Box<[u8]>,
    len: usize,
    _permit: OwnedSemaphorePermit,
}
impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.data[..self.len]
    }
}
type Reading = Pin<Box<dyn Future<Output = io::Result<Bytes>> + Send>>;
pub(crate) struct FileBody {
    file: Option<Arc<FileLease>>,
    resources: Arc<Resources>,
    parts: VecDeque<Part>,
    pending: Option<Reading>,
    remaining: u64,
}
impl FileBody {
    pub(super) fn new(file: FileLease, resources: Arc<Resources>, parts: Vec<Part>) -> Self {
        Self {
            file: Some(Arc::new(file)),
            resources,
            remaining: parts.iter().map(Part::len).sum(),
            parts: parts.into(),
            pending: None,
        }
    }
    fn end(&mut self) {
        self.file = None;
        self.parts.clear();
        self.pending = None;
        self.remaining = 0;
    }
}
impl Body for FileBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        loop {
            if let Some(pending) = &mut self.pending {
                let result = match pending.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result,
                };
                self.pending = None;
                match result {
                    Ok(bytes) => {
                        self.remaining -= bytes.len() as u64;
                        if self.remaining == 0 {
                            self.end();
                        }
                        return Poll::Ready(Some(Ok(Frame::data(bytes))));
                    }
                    Err(error) => {
                        self.end();
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
            match self.parts.pop_front() {
                None => {
                    self.end();
                    return Poll::Ready(None);
                }
                Some(Part::Literal(bytes)) => {
                    self.remaining -= bytes.len() as u64;
                    if self.remaining == 0 {
                        self.end();
                    }
                    return Poll::Ready(Some(Ok(Frame::data(bytes))));
                }
                Some(Part::Span { start, len }) => {
                    if len == 0 {
                        continue;
                    }
                    let read = len.min(CHUNK_BYTES as u64) as usize;
                    if len > read as u64 {
                        self.parts.push_front(Part::Span {
                            start: start + read as u64,
                            len: len - read as u64,
                        });
                    }
                    let file = self.file.as_ref().unwrap().clone();
                    let resources = self.resources.clone();
                    self.pending = Some(Box::pin(async move {
                        tokio::time::timeout(Duration::from_secs(15), async {
                            let permit = tokio::select! { biased;
                                _ = resources.stopped.cancelled() => return Err(stopped()),
                                permit = resources.chunks.clone().acquire_owned() => permit.map_err(|_| stopped())?,
                            };
                            resources.run(move || {
                                // Fixed backing allocation is charged before allocating, even
                                // after the HTTP body/worker drops or a small slice survives.
                                let mut chunk = Chunk {data: vec![0; CHUNK_BYTES].into_boxed_slice(), len: read, _permit: permit};
                                file.file.read_exact_at(&mut chunk.data[..read], start)?;
                                Ok(Bytes::from_owner(chunk))
                            }).await
                        }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "asset read timed out"))?
                    }));
                }
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_caller_keeps_worker_admitted_and_shutdown_joins_it() {
        let resources = Resources::new();
        let worker = resources.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            worker
                .run(move || {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        caller.abort();
        let _ = caller.await;
        assert_eq!(resources.workers.available_permits(), WORKERS - 1);
        let closing = resources.clone();
        let mut shutdown = tokio::spawn(async move {
            closing.shutdown().await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert!(resources.run(|| Ok(())).await.is_err());
    }
}
