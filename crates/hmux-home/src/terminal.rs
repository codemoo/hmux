//! One bounded terminal job. The shared peer never waits on PTY input or output
//! credit. Its owner joins the child and guarded view cleanup before returning.
use crate::{
    observation,
    peer::{self, Error},
    pty, view,
};
use bytes::Bytes;
use hmux_core::command::CommandRunner;
use hmux_model::SessionIdentity;
use hmux_protocol::{
    flow,
    protobuf::{types as p, Negotiated},
    transport::Sender,
};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant as StdInstant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Notify, OwnedSemaphorePermit},
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;

const INPUT_FRAMES: usize = 32;
const CREDIT_WAIT: Duration = Duration::from_secs(40);
const UNAVAILABLE: &str = "Home operation unavailable";
static TERMINAL_COMMANDS: OnceLock<CommandRunner> = OnceLock::new();

pub(crate) enum Input {
    Data(Bytes),
    Resize(u16, u16),
    Refresh,
}
struct Credit {
    window: Mutex<flow::OutputWindow>,
    changed: Notify,
}
impl Credit {
    fn acknowledge(&self, n: i64) -> bool {
        let accepted = self
            .window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .acknowledge(n);
        if accepted {
            self.changed.notify_one();
        }
        accepted
    }
    async fn reserve(&self, n: usize, stop: &CancellationToken) -> Result<(), End> {
        let work = async {
            loop {
                let changed = self.changed.notified();
                if stop.is_cancelled() {
                    return Err(End::Closed);
                }
                if self
                    .window
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .reserve(n, StdInstant::now())
                {
                    return Ok(());
                }
                tokio::select! { _=stop.cancelled()=>return Err(End::Closed), _=changed=>{} }
            }
        };
        timeout(CREDIT_WAIT, work)
            .await
            .unwrap_or(Err(End::Stalled))
    }
}

pub(crate) struct Handle {
    pub(crate) stop: CancellationToken,
    input: mpsc::Sender<Input>,
    credit: Option<Arc<Credit>>,
}
impl Handle {
    pub(crate) fn input(&self, input: Input) {
        // Protobuf Bytes can refer to a full message; retain only the exact
        // bounded input payload in the queue, never its unknown backing owner.
        let input = match input {
            Input::Data(data) => Input::Data(Bytes::copy_from_slice(&data)),
            other => other,
        };
        if self.input.try_send(input).is_err() {
            self.stop.cancel();
        }
    }
    pub(crate) fn acknowledge(&self, n: i64) {
        if !self
            .credit
            .as_ref()
            .is_some_and(|credit| credit.acknowledge(n))
        {
            self.stop.cancel();
        }
    }
}
pub(crate) fn channel(
    flow_control: bool,
    parent: &CancellationToken,
) -> (Handle, mpsc::Receiver<Input>) {
    let (input, receiver) = mpsc::channel(INPUT_FRAMES);
    let credit = flow_control.then(|| {
        Arc::new(Credit {
            window: Mutex::new(flow::OutputWindow::default()),
            changed: Notify::new(),
        })
    });
    (
        Handle {
            stop: parent.child_token(),
            input,
            credit,
        },
        receiver,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Closed,
    Stalled,
    Transport,
}
async fn output<R: AsyncRead + Unpin>(
    mut reader: R,
    credit: Option<Arc<Credit>>,
    sender: &Sender,
    protocol: Negotiated,
    id: &str,
    stop: &CancellationToken,
) -> End {
    let mut buffer = vec![0u8; flow::CHUNK];
    loop {
        let read = tokio::select! { biased; _=stop.cancelled()=>return End::Closed, value=reader.read(&mut buffer)=>value };
        let n = match read {
            Ok(0) | Err(_) => return End::Closed,
            Ok(n) => n,
        };
        if let Some(credit) = &credit {
            if let Err(end) = credit.reserve(n, stop).await {
                return end;
            }
        }
        let body = p::envelope::Body::TerminalOutput(p::Data {
            id: id.into(),
            data: Bytes::copy_from_slice(&buffer[..n]),
        });
        if peer::send(sender, protocol, body, stop.clone())
            .await
            .is_err()
        {
            return if stop.is_cancelled() {
                End::Closed
            } else {
                End::Transport
            };
        }
    }
}
struct InputContext<'a> {
    view: &'a view::OwnedView,
    runner: &'a CommandRunner,
    pid: u32,
    sender: &'a Sender,
    protocol: Negotiated,
    id: &'a str,
    stop: &'a CancellationToken,
}
async fn input(
    mut writer: pty::WriteHalf,
    mut receiver: mpsc::Receiver<Input>,
    context: InputContext<'_>,
) -> End {
    let InputContext {
        view,
        runner,
        pid,
        sender,
        protocol,
        id,
        stop,
    } = context;
    let mut refreshed = None;
    loop {
        let value = tokio::select! { biased; _=stop.cancelled()=>return End::Closed,value=receiver.recv()=>value };
        match value {
            None => return End::Closed,
            Some(Input::Data(data)) => {
                let result = tokio::select! { biased; _=stop.cancelled()=>return End::Closed,value=writer.write_all(&data)=>value };
                if result.is_err() {
                    return End::Closed;
                }
            }
            Some(Input::Resize(cols, rows)) => {
                if writer.resize(cols, rows).is_err() {
                    return End::Closed;
                }
            }
            Some(Input::Refresh) => {
                let now = Instant::now();
                if refreshed.is_some_and(|last| now.duration_since(last) < Duration::from_secs(1)) {
                    continue;
                }
                refreshed = Some(now);
                let error = if crate::refresh::run(view, runner, pid, stop).await.is_ok() {
                    ""
                } else {
                    "Terminal refresh unavailable"
                };
                if stop.is_cancelled() {
                    return End::Closed;
                }
                let body = p::envelope::Body::RefreshResult(p::Response {
                    id: id.into(),
                    result: None,
                    error: error.into(),
                });
                if peer::send(sender, protocol, body, stop.clone())
                    .await
                    .is_err()
                {
                    return if stop.is_cancelled() {
                        End::Closed
                    } else {
                        End::Transport
                    };
                }
            }
        }
    }
}

pub(crate) struct Job {
    pub(crate) request: p::TerminalOpen,
    pub(crate) target: view::Target,
    pub(crate) sender: Sender,
    pub(crate) protocol: Negotiated,
    pub(crate) link_stop: CancellationToken,
    pub(crate) permit: OwnedSemaphorePermit,
    pub(crate) reporter: Option<observation::Reporter>,
}
impl Job {
    pub(crate) async fn run(
        self,
        handle: &Handle,
        receiver: mpsc::Receiver<Input>,
    ) -> Result<(), Error> {
        let Self {
            request,
            target,
            sender,
            protocol,
            link_stop,
            permit,
            reporter,
        } = self;
        // Catalog collection must never compete with slow creates or refreshes.
        // One process-wide pool serves all admitted terminal jobs; view cleanup
        // has its own independently bounded pool.
        let runner = TERMINAL_COMMANDS
            .get_or_init(|| {
                CommandRunner::new(hmux_protocol::wire::MAX_TERMINALS)
                    .expect("nonzero terminal limit")
            })
            .clone();
        let id = request.id;
        let stop = handle.stop.clone();
        let session = request.session.expect("validated wire identity");
        let opened = view::open_reported(
            target,
            runner.clone(),
            SessionIdentity {
                id: session.id,
                created_at: session.created_at,
            },
            stop.clone(),
            reporter,
        )
        .await;
        let opened = match opened {
            Ok(view) => {
                let (executable, args) = view.attach_command();
                match pty::spawn(&executable, &args, request.cols as u16, request.rows as u16) {
                    Ok(pty) => Ok((view, pty)),
                    Err(_) => {
                        let _ = view.close().await;
                        Err(())
                    }
                }
            }
            Err(_) => Err(()),
        };
        // Startup admission is independent from the terminal's lifetime slots.
        drop(permit);
        let (view, terminal) = match opened {
            Ok(pair) => pair,
            Err(()) => {
                if stop.is_cancelled() {
                    return Ok(());
                }
                return peer::send(
                    &sender,
                    protocol,
                    p::envelope::Body::Response(p::Response {
                        id,
                        result: None,
                        error: UNAVAILABLE.into(),
                    }),
                    link_stop,
                )
                .await;
            }
        };
        let pid = terminal.pid();
        let response = peer::send(
            &sender,
            protocol,
            p::envelope::Body::Response(p::Response {
                id: id.clone(),
                result: Some(p::response::Result::Ok(p::Empty {})),
                error: String::new(),
            }),
            stop.clone(),
        )
        .await;
        let end = if response.is_err() || stop.is_cancelled() {
            let end = if stop.is_cancelled() {
                End::Closed
            } else {
                End::Transport
            };
            terminal.close().await;
            end
        } else {
            let (master, child) = terminal.into_parts();
            let (read, write) = master.into_split();
            let end = {
                let input = input(
                    write,
                    receiver,
                    InputContext {
                        view: &view,
                        runner: &runner,
                        pid,
                        sender: &sender,
                        protocol,
                        id: &id,
                        stop: &stop,
                    },
                );
                let output = output(read, handle.credit.clone(), &sender, protocol, &id, &stop);
                tokio::pin!(input, output);
                let (end, input_done, output_done) = tokio::select! {
                    biased;
                    _=stop.cancelled()=>(End::Closed,false,false),
                    value=&mut input=>(value,true,false),
                    value=&mut output=>(value,false,true),
                };
                stop.cancel();
                // An input refresh may own an in-flight tmux/ps command. Poll
                // cooperative cancellation through reaping rather than dropping
                // its future when another I/O half finishes first.
                if !input_done {
                    let _ = input.await;
                }
                if !output_done {
                    let _ = output.await;
                }
                end
            };
            // Both descriptor halves have now dropped and requested direct-child
            // cancellation; keep its owner and view until actual reaping finishes.
            child.close().await;
            end
        };
        stop.cancel();
        let cleanup_failed = view.close().await.is_err();
        if end == End::Transport {
            return Err(Error::Transport);
        }
        if link_stop.is_cancelled() {
            return Ok(());
        }
        peer::send(
            &sender,
            protocol,
            p::envelope::Body::TerminalExit(p::Response {
                id,
                result: None,
                error: if cleanup_failed {
                    "view-cleanup-failed".into()
                } else if end == End::Stalled {
                    "output-stalled".into()
                } else {
                    String::new()
                },
            }),
            link_stop,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn exhausted_credit_times_out_and_cancel_does_not_release_credit() {
        let credit = Credit {
            window: Mutex::new(flow::OutputWindow::default()),
            changed: Notify::new(),
        };
        let stop = CancellationToken::new();
        for _ in 0..flow::FRAMES {
            credit.reserve(1, &stop).await.unwrap();
        }
        let started = Instant::now();
        assert_eq!(credit.reserve(1, &stop).await, Err(End::Stalled));
        assert_eq!(Instant::now() - started, CREDIT_WAIT);
        assert!(!credit.acknowledge(2));
        assert!(credit.acknowledge(1));
        credit.reserve(1, &stop).await.unwrap();
        stop.cancel();
        assert_eq!(credit.reserve(1, &stop).await, Err(End::Closed));
        assert_eq!(
            credit.window.lock().unwrap().retained_frames(),
            flow::FRAMES
        );
    }
}
