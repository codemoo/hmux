//! A single bounded, nonblocking diagnostic queue per process. Native writes
//! are serialized on the shared blocking pool; no per-tab logger or timer.
use hmux_gateway::observation;
use hmux_home::connector::Lifecycle;
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const RECORDS: usize = 64;
#[derive(Clone, Copy)]
pub enum Event {
    Message(&'static str),
    Home(Lifecycle),
    Gateway(observation::Event),
}
struct Record {
    seconds: i64,
    event: Event,
}
#[derive(Clone)]
pub struct Sender {
    channel: mpsc::Sender<Record>,
    dropped: Arc<AtomicU64>,
}
impl Sender {
    pub fn send(&self, event: Event) {
        if self
            .channel
            .try_send(Record {
                seconds: chrono::Utc::now().timestamp(),
                event,
            })
            .is_err()
        {
            let _ = self
                .dropped
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n.saturating_add(1))
                });
        }
    }
    pub fn reporter(&self) -> observation::Reporter {
        let sender = self.clone();
        Arc::new(move |event| sender.send(Event::Gateway(event)))
    }
}
pub struct Log {
    sender: Sender,
    stop: CancellationToken,
    worker: Option<JoinHandle<io::Result<()>>>,
}
impl Log {
    pub fn start(writer: impl Write + Send + 'static) -> Self {
        let (channel, mut records) = mpsc::channel::<Record>(RECORDS);
        let dropped = Arc::new(AtomicU64::new(0));
        let sender = Sender {
            channel,
            dropped: dropped.clone(),
        };
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let worker = tokio::spawn(async move {
            let mut writer = writer;
            loop {
                let record = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { records.close(); records.recv().await },
                    record = records.recv() => record,
                };
                let count = dropped.swap(0, Ordering::Relaxed);
                if record.is_none() && count == 0 {
                    break;
                }
                let (next, result) = tokio::task::spawn_blocking(move || {
                    let result = (|| {
                        if count > 0 {
                            writeln!(writer, "diagnostic records dropped={count}")?;
                        }
                        if let Some(record) = record {
                            let timestamp = chrono::DateTime::from_timestamp(record.seconds, 0)
                                .unwrap_or_default()
                                .format("%Y/%m/%d %H:%M:%S");
                            match record.event {
                                Event::Message(message) => {
                                    writeln!(writer, "{timestamp} {message}")
                                }
                                Event::Home(state) => {
                                    writeln!(writer, "{timestamp} Home connector: {state:?}")
                                }
                                Event::Gateway(event) => writeln!(writer, "{timestamp} {event}"),
                            }?;
                        }
                        writer.flush()
                    })();
                    (writer, result)
                })
                .await
                .map_err(io::Error::other)?;
                writer = next;
                result?;
            }
            Ok(())
        });
        Self {
            sender,
            stop,
            worker: Some(worker),
        }
    }
    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }
    pub async fn shutdown(mut self) -> io::Result<()> {
        self.stop.cancel();
        self.worker
            .take()
            .expect("owned log worker")
            .await
            .map_err(io::Error::other)?
    }
}
impl Drop for Log {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn saturated_diagnostics_stay_bounded_report_loss_and_join_on_shutdown() {
        let buffer = Buffer(Arc::default());
        let log = Log::start(buffer.clone());
        let sender = log.sender();
        for _ in 0..1000 {
            sender.send(Event::Message("synthetic event"));
        }
        assert_eq!(sender.channel.capacity(), 0);
        log.shutdown().await.unwrap();
        let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("synthetic event").count(), 64);
        assert!(output.contains("dropped=936"));
        assert!(sender.channel.is_closed());
    }
}
