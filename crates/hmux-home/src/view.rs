//! Owned disposable tmux grouped views. This module never attaches a client or
//! signals an original session. Every cleanup checks the fresh ownership nonce.
use crate::{
    catalog::{TmuxCatalogReader, TmuxSocket},
    observation::{self, Event, Reason, Stage},
};
use hmux_core::command::{CommandRunner, CommandSpec, RunErrorKind};
use hmux_model::{validate_session_id, SessionIdentity};
use std::{
    ffi::OsString,
    fmt,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    sync::{oneshot, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 4096;
static VIEW_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static CLEANUP_RUNNER: OnceLock<CommandRunner> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Entropy,
    Changed,
    Command,
    Cancelled,
    Cleanup,
    Worker,
}

/// Validated command destination. The same executable/socket must be used for
/// setup, attachment, refresh and cleanup; no administrative shell is involved.
#[derive(Clone)]
pub struct Target {
    executable: PathBuf,
    socket: Option<TmuxSocket>,
}
impl Target {
    pub(crate) fn executable_path(&self) -> &std::path::Path {
        &self.executable
    }
    pub fn new(executable: PathBuf, socket: Option<TmuxSocket>) -> Result<Self, Error> {
        TmuxCatalogReader::new(executable.clone(), socket.clone(), SETUP_TIMEOUT)
            .map_err(|_| Error::Invalid)?;
        Ok(Self { executable, socket })
    }
    fn args(&self, args: Vec<String>) -> Vec<OsString> {
        self.args_os(args.into_iter().map(OsString::from).collect())
    }
    fn args_os(&self, args: Vec<OsString>) -> Vec<OsString> {
        let mut result = Vec::with_capacity(args.len() + 2);
        match &self.socket {
            Some(TmuxSocket::Name(name)) => result.extend([OsString::from("-L"), name.into()]),
            Some(TmuxSocket::Path(path)) => result.extend([OsString::from("-S"), path.into()]),
            None => {}
        }
        result.extend(args);
        result
    }
    pub(crate) async fn command(
        &self,
        runner: &CommandRunner,
        args: Vec<String>,
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Vec<u8>, Error> {
        self.command_os(
            runner,
            args.into_iter().map(OsString::from).collect(),
            stop,
            deadline,
        )
        .await
    }
    pub(crate) async fn command_os(
        &self,
        runner: &CommandRunner,
        args: Vec<OsString>,
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Vec<u8>, Error> {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Command);
        }
        let spec = CommandSpec::new(self.executable.clone(), OUTPUT_LIMIT, remaining)
            .args(self.args_os(args));
        let (sender, receiver) = oneshot::channel();
        let work = runner.run_cancelable(spec, receiver);
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = stop.cancelled() => {
                drop(sender);
                let _ = work.await; // The direct tmux command must be reaped.
                Err(Error::Cancelled)
            },
            result=&mut work => result.map(|result|result.stdout).map_err(|_|Error::Command),
        }
    }
    async fn cleanup(&self, runner: &CommandRunner, identity: &Identity) -> Result<(), Error> {
        let spec = CommandSpec::new(self.executable.clone(), OUTPUT_LIMIT, CLEANUP_TIMEOUT)
            .args(self.args(identity.cleanup()));
        match runner.run(spec).await {
            Ok(_) => Ok(()),
            // The view-local detach hook may have removed the session already.
            // Only a verified missing session/server is an idempotent success.
            Err(error) if crate::catalog::no_tmux_server(&error) => Ok(()),
            Err(error)
                if error.kind() == RunErrorKind::Exit
                    && error.exit_code() == Some(1)
                    && error.stderr()
                        == Some(format!("can't find session: {}\n", identity.name).as_bytes()) =>
            {
                Ok(())
            }
            Err(_) => Err(Error::Cleanup),
        }
    }
}

#[derive(Clone)]
struct Identity {
    name: String,
    nonce: String,
}
impl Identity {
    fn new() -> Result<Self, Error> {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).map_err(|_| Error::Entropy)?;
        let nonce = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        Ok(Self {
            name: format!("hmux-app-view-{}-{nonce}", std::process::id()),
            nonce,
        })
    }
    fn condition(&self, detached: bool) -> String {
        // new-session installs this environment value as part of creating the
        // session. Cleanup remains provable if later option/hook commands fail.
        let owned = format!(
            "#{{&&:#{{==:#{{session_name}},{}}},#{{==:#{{HMUX_VIEW_OWNER}},{}}}}}",
            self.name, self.nonce
        );
        if detached {
            format!("#{{&&:{owned},#{{==:#{{session_attached}},0}}}}")
        } else {
            owned
        }
    }
    fn create(&self, session: &SessionIdentity) -> Vec<String> {
        [
            "new-session",
            "-d",
            "-s",
            &self.name,
            "-e",
            &format!("HMUX_VIEW_OWNER={}", self.nonce),
            "-t",
            &session.id,
            ";",
            "set-option",
            "-t",
            &self.name,
            "@hmux_app_view",
            "1",
            ";",
            "set-option",
            "-t",
            &self.name,
            "status",
            "off",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
    fn cleanup(&self) -> Vec<String> {
        vec![
            "if-shell".into(),
            "-F".into(),
            "-t".into(),
            self.name.clone(),
            self.condition(false),
            format!("kill-session -t {}", self.name),
        ]
    }
    fn hook(&self) -> Vec<String> {
        vec![
            "set-hook".into(),
            "-t".into(),
            self.name.clone(),
            "client-detached".into(),
            format!(
                "if-shell -F -t {} '{}' 'kill-session -t {}'",
                self.name,
                self.condition(true),
                self.name
            ),
        ]
    }
}

/// Dropping a view requests guarded cleanup in its already-running owner.
/// Use `close().await` to observe completion while keeping the runtime alive.
pub struct OwnedView {
    target: Target,
    identity: Identity,
    stop: CancellationToken,
    done: oneshot::Receiver<Result<(), Error>>,
}
impl fmt::Debug for OwnedView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OwnedView([redacted])")
    }
}
impl Drop for OwnedView {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl OwnedView {
    pub(crate) fn refresh_destination(&self) -> (&Target, &str, &str) {
        (&self.target, &self.identity.name, &self.identity.nonce)
    }

    pub(crate) fn closing_token(&self) -> &CancellationToken {
        &self.stop
    }

    /// Shared attachment deliberately omits `-d`: existing clients stay attached.
    pub fn attach_command(&self) -> (PathBuf, Vec<OsString>) {
        (
            self.target.executable.clone(),
            self.target.args(vec![
                "attach-session".into(),
                "-t".into(),
                self.identity.name.clone(),
            ]),
        )
    }
    pub async fn close(mut self) -> Result<(), Error> {
        self.stop.cancel();
        (&mut self.done).await.map_err(|_| Error::Worker)?
    }
}

pub async fn open(
    target: Target,
    runner: CommandRunner,
    session: SessionIdentity,
    shutdown: CancellationToken,
) -> Result<OwnedView, Error> {
    open_reported(target, runner, session, shutdown, None).await
}

pub(crate) async fn open_reported(
    target: Target,
    runner: CommandRunner,
    session: SessionIdentity,
    shutdown: CancellationToken,
    reporter: Option<observation::Reporter>,
) -> Result<OwnedView, Error> {
    if validate_session_id(&session.id).is_err() || session.created_at < 1 {
        return Err(Error::Invalid);
    }
    if shutdown.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let permit = VIEW_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(hmux_protocol::wire::MAX_TERMINALS)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Busy)?;
    let identity = Identity::new()?;
    let stop = shutdown.child_token();
    let guard = stop.clone().drop_guard();
    let (ready_sender, ready) = oneshot::channel();
    let (done_sender, mut done) = oneshot::channel();
    tokio::spawn(owner(
        target.clone(),
        runner,
        session,
        identity.clone(),
        stop.clone(),
        permit,
        ready_sender,
        done_sender,
        reporter,
    ));
    let result = ready.await.map_err(|_| Error::Worker)?;
    if let Err(error) = result {
        let _ = (&mut done).await;
        return Err(error);
    }
    let view = OwnedView {
        target,
        identity,
        stop,
        done,
    };
    guard.disarm();
    if shutdown.is_cancelled() {
        let _ = view.close().await;
        return Err(Error::Cancelled);
    }
    Ok(view)
}

pub(crate) async fn verify(
    target: &Target,
    runner: &CommandRunner,
    session: &SessionIdentity,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<(), Error> {
    let raw = target
        .command(
            runner,
            vec![
                "display-message".into(),
                "-p".into(),
                "-t".into(),
                session.id.clone(),
                "#{session_created}".into(),
            ],
            stop,
            deadline,
        )
        .await?;
    if std::str::from_utf8(&raw)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        != Some(session.created_at)
    {
        return Err(Error::Changed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn owner(
    target: Target,
    runner: CommandRunner,
    session: SessionIdentity,
    identity: Identity,
    stop: CancellationToken,
    permit: OwnedSemaphorePermit,
    ready: oneshot::Sender<Result<(), Error>>,
    done: oneshot::Sender<Result<(), Error>>,
    reporter: Option<observation::Reporter>,
) {
    let deadline = Instant::now() + SETUP_TIMEOUT;
    let mut attempted = false;
    let setup = async {
        verify(&target, &runner, &session, &stop, deadline).await?;
        attempted = true;
        target
            .command(&runner, identity.create(&session), &stop, deadline)
            .await?;
        verify(&target, &runner, &session, &stop, deadline).await?;
        target
            .command(&runner, identity.hook(), &stop, deadline)
            .await?;
        Ok(())
    }
    .await;
    let mut ready = Some(ready);
    if setup.is_ok() {
        if ready.take().unwrap().send(Ok(())).is_err() {
            stop.cancel();
        }
        stop.cancelled().await;
    }
    let cleanup_started = std::time::Instant::now();
    let cleanup = if attempted {
        // One dedicated cleanup slot for each admitted view; setup/request
        // pressure cannot consume this independent, process-wide pool.
        let runner = CLEANUP_RUNNER.get_or_init(|| {
            CommandRunner::new(hmux_protocol::wire::MAX_TERMINALS).expect("nonzero view limit")
        });
        target.cleanup(runner, &identity).await
    } else {
        Ok(())
    };
    if cleanup.is_err() {
        // Unknown cleanup outcome must not admit unlimited replacement views.
        // Quarantine this slot for this process's lifetime, without a resident
        // retry task. Report the failure to the caller for operational recovery.
        permit.forget();
        // Report here, where cleanup is owned, including cancelled startup or a
        // dropped terminal job. Never record tmux names, paths or raw stderr.
        if let Some(report) = reporter {
            report(Event::new(
                Stage::ViewCleanup,
                None,
                Reason::Quarantined,
                cleanup_started,
            ));
        }
    } else {
        drop(permit);
    }
    if let Some(ready) = ready {
        let _ = ready.send(cleanup.and(setup));
    }
    let _ = done.send(cleanup);
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
