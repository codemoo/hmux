//! Candidate Home connector owner. Preparation is synchronous and holds the
//! lifetime singleton before any trust loading or network work. The private
//! owner survives caller abort, cancels the connected peer and joins cleanup
//! before releasing the lock. Usage collection is shared across reconnects;
//! service installation remains outside this owner.
use crate::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    dial::{self, Client, Endpoint},
    filestage::Store,
    inspection::Inspector,
    metrics, observation, peer, sessions,
    singleton::{self, ConnectorLock},
    upgrade, upload, usage, usage_config,
};
use hmux_core::command::CommandRunner;
use hmux_model::SCHEMA_VERSION;
use hmux_protocol::transport;
use std::{fmt, future::Future, sync::Arc, time::Duration};
use tokio::{sync::watch, time::sleep};
use tokio_util::sync::CancellationToken;

const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Fixed lifecycle categories. The one-slot watch retains only the latest
/// state and never stores an endpoint, bearer, remote response or raw error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Prepared,
    Starting,
    StartupFailed(dial::Error),
    Dialing,
    Connected,
    DialFailed(dial::Error),
    PeerFailed(peer::Error),
    PeerEnded,
    Stopped,
}

/// Fixed errors contain no private configuration or network details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Config,
    Endpoint,
    Bearer,
    Lock(singleton::Error),
    Startup(dial::Error),
    Recovery,
    Worker,
}

/// Latest-state observer. It returns copied values, never a watch read guard
/// that could be held across an await and stall the connector owner.
pub struct LifecycleObserver {
    receiver: watch::Receiver<Lifecycle>,
}
impl LifecycleObserver {
    pub fn latest(&self) -> Lifecycle {
        *self.receiver.borrow()
    }
    pub async fn changed(&mut self) -> Option<Lifecycle> {
        self.receiver.changed().await.ok()?;
        Some(*self.receiver.borrow_and_update())
    }
}

/// One Home owner. This type is not cloneable; its Debug output is redacted.
pub struct Prepared {
    config: HomeConfig,
    endpoint: String,
    bearer: String,
    lock: ConnectorLock,
    lifecycle: watch::Sender<Lifecycle>,
    store: Option<Arc<Store>>,
    session_context: Option<Arc<sessions::Context>>,
    inspector: Option<Arc<Inspector>>,
    completions: bool,
    metrics: Option<Arc<metrics::Collector>>,
    usage_options: Option<usage_config::Options>,
    workspace: Option<crate::workspace::Workspace>,
    providers: Option<Arc<crate::providers::ProviderService>>,
    recovery: Option<crate::recovery::Store>,
    reporter: Option<observation::Reporter>,
}
impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Prepared([redacted])")
    }
}
impl Prepared {
    /// Validate before locking, then acquire the shared Go/Rust connector lock
    /// before any async work. Dropping this owner before polling `run` is inert.
    pub fn prepare(config: HomeConfig, endpoint: &str, bearer: &str) -> Result<Self, Error> {
        if config.role != "home"
            || config.schema_version != SCHEMA_VERSION
            || !config.inventory_path.is_absolute()
        {
            return Err(Error::Config);
        }
        Endpoint::parse(endpoint).map_err(|_| Error::Endpoint)?;
        if !upgrade::valid_bearer(bearer) {
            return Err(Error::Bearer);
        }
        let lock = ConnectorLock::acquire(&config.state_dir).map_err(Error::Lock)?;
        let (lifecycle, _) = watch::channel(Lifecycle::Prepared);
        Ok(Self {
            config,
            endpoint: endpoint.to_owned(),
            bearer: bearer.to_owned(),
            lock,
            lifecycle,
            store: None,
            session_context: None,
            inspector: None,
            completions: false,
            metrics: None,
            usage_options: None,
            workspace: None,
            providers: None,
            recovery: None,
            reporter: None,
        })
    }

    /// Explicit candidate upload capability. Library/test callers leave it
    /// disabled unless they supply an isolated private spool.
    pub fn with_upload_store(mut self, store: Arc<Store>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_session_context(mut self, context: Arc<sessions::Context>) -> Self {
        self.session_context = Some(context);
        self
    }

    pub fn with_inspector(mut self, inspector: Arc<Inspector>) -> Self {
        self.inspector = Some(inspector);
        self
    }

    pub fn with_completion_notifications(mut self) -> Self {
        self.completions = true;
        self
    }

    pub fn with_metrics(mut self, collector: Arc<metrics::Collector>) -> Self {
        self.metrics = Some(collector);
        self
    }

    /// Captures source configuration; collection starts once after connector startup.
    pub fn with_usage(mut self, options: usage_config::Options) -> Self {
        self.usage_options = Some(options);
        self
    }

    /// Shared tabs stay owned by this connector across peer reconnects.
    pub fn with_workspace(mut self, workspace: crate::workspace::Workspace) -> Self {
        self.workspace = Some(workspace);
        self
    }

    pub fn with_providers(mut self, providers: Arc<crate::providers::ProviderService>) -> Self {
        self.providers = Some(providers);
        self
    }
    pub fn with_recovery(mut self, recovery: crate::recovery::Store) -> Self {
        self.recovery = Some(recovery);
        self
    }
    pub fn with_reporter(mut self, reporter: observation::Reporter) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// A bounded latest-state observer; receivers never control the owner.
    pub fn lifecycle(&self) -> LifecycleObserver {
        LifecycleObserver {
            receiver: self.lifecycle.subscribe(),
        }
    }

    /// Own reconnects until shutdown. Build a fresh read-only catalog reader
    /// for each connected peer; the runner keeps shared command admission.
    /// Cancel shutdown and await this future for completed cleanup. Caller
    /// abort also requests cancellation, while its private task keeps the lock.
    pub async fn run(
        mut self,
        catalog: TmuxCatalogReader,
        runner: CommandRunner,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        let driver = Production {
            client: None,
            store: self.store.clone(),
            session_context: self.session_context.clone(),
            inspector: self.inspector.clone(),
            completions: self.completions,
            metrics: self.metrics.clone(),
            usage_options: self.usage_options.take(),
            usage: None,
            workspace: self.workspace.clone(),
            providers: self.providers.clone(),
            recovery: self.recovery.clone(),
            reporter: self.reporter.clone(),
            checkpoint: None,
            catalog,
            runner,
        };
        self.run_with(driver, shutdown).await
    }

    async fn run_with<D: Driver>(
        self,
        driver: D,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        let stop = shutdown.child_token();
        let _cancel_on_caller_drop = stop.clone().drop_guard();
        tokio::spawn(run_owned(self, driver, stop))
            .await
            .map_err(|_| Error::Worker)?
    }
}

trait Driver: Send + 'static {
    fn prepare_home(
        &mut self,
        _stop: &CancellationToken,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        async { Ok(()) }
    }
    fn start_collectors(&mut self, _stop: &CancellationToken) {}
    fn stop_collectors(&mut self) -> impl Future<Output = Result<(), Error>> + Send {
        async { Ok(()) }
    }
    type Connected: Send + 'static;
    fn startup(&mut self) -> impl Future<Output = Result<(), dial::Error>> + Send;
    fn dial<'a>(
        &'a mut self,
        endpoint: &'a str,
        bearer: &'a str,
        stop: &'a CancellationToken,
    ) -> impl Future<Output = Result<Self::Connected, dial::Error>> + Send + 'a;
    fn serve(
        &mut self,
        connection: Self::Connected,
        config: HomeConfig,
        stop: CancellationToken,
    ) -> impl Future<Output = Result<(), peer::Error>> + Send;
}

struct Production {
    checkpoint: Option<crate::recovery::Checkpoint>,
    usage_options: Option<usage_config::Options>,
    workspace: Option<crate::workspace::Workspace>,
    providers: Option<Arc<crate::providers::ProviderService>>,
    recovery: Option<crate::recovery::Store>,
    reporter: Option<observation::Reporter>,
    usage: Option<usage::Collector>,
    session_context: Option<Arc<sessions::Context>>,
    inspector: Option<Arc<Inspector>>,
    completions: bool,
    metrics: Option<Arc<metrics::Collector>>,
    store: Option<Arc<Store>>,
    client: Option<Client>,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
}
impl Driver for Production {
    type Connected = transport::Connection;
    async fn prepare_home(&mut self, stop: &CancellationToken) -> Result<(), Error> {
        if let Some(recovery) = &self.recovery {
            self.checkpoint = Some(
                recovery
                    .prepare_checkpoint(stop.clone())
                    .await
                    .map_err(|_| Error::Recovery)?,
            );
        }
        Ok(())
    }
    fn start_collectors(&mut self, stop: &CancellationToken) {
        self.usage = self.usage_options.take().map(|options| {
            usage::Collector::start(
                options,
                self.client.as_ref().expect("startup completed").clone(),
                stop,
            )
        });
    }
    async fn stop_collectors(&mut self) -> Result<(), Error> {
        if let Some(checkpoint) = self.checkpoint.take() {
            checkpoint.shutdown().await;
        }
        if let Some(providers) = &self.providers {
            providers.shutdown().await;
        }
        if let Some(workspace) = &self.workspace {
            workspace.shutdown().await;
        }
        if let Some(usage) = self.usage.take() {
            usage.shutdown().await.map_err(|_| Error::Worker)?;
        }
        Ok(())
    }
    async fn startup(&mut self) -> Result<(), dial::Error> {
        self.client = Some(Client::new().await?);
        Ok(())
    }
    async fn dial(
        &mut self,
        endpoint: &str,
        bearer: &str,
        stop: &CancellationToken,
    ) -> Result<Self::Connected, dial::Error> {
        self.client
            .as_ref()
            .expect("startup completed")
            .connect(endpoint, bearer, stop, None)
            .await
    }
    async fn serve(
        &mut self,
        connection: Self::Connected,
        config: HomeConfig,
        stop: CancellationToken,
    ) -> Result<(), peer::Error> {
        peer::run_connected_with_services(
            connection,
            config,
            self.catalog.clone(),
            self.runner.clone(),
            peer::Services {
                uploads: self.store.clone(),
                sessions: self.session_context.clone(),
                inspector: self.inspector.clone(),
                completions: self.completions,
                metrics: self.metrics.clone(),
                usage: self.usage.as_ref().map(usage::Collector::subscribe),
                workspace: self.workspace.clone(),
                providers: self.providers.clone(),
                recovery: self.recovery.clone(),
                reporter: self.reporter.clone(),
                usage_refresh: self.usage.as_ref().map(usage::Collector::refresh_handle),
            },
            stop,
        )
        .await
    }
}

fn publish(status: &watch::Sender<Lifecycle>, next: Lifecycle) {
    if *status.borrow() != next {
        status.send_replace(next);
    }
}
async fn run_owned<D: Driver>(
    prepared: Prepared,
    mut driver: D,
    stop: CancellationToken,
) -> Result<(), Error> {
    let _cancel_on_unwind = stop.clone().drop_guard();
    let Prepared {
        config,
        endpoint,
        bearer,
        lock: _lock,
        lifecycle,
        store,
        session_context: _,
        inspector: _,
        completions: _,
        metrics: _,
        usage_options: _,
        workspace: _,
        providers: _,
        recovery: _,
        reporter: _,
    } = prepared;
    publish(&lifecycle, Lifecycle::Starting);
    let startup = tokio::select! {
        biased;
        _ = stop.cancelled() => { publish(&lifecycle, Lifecycle::Stopped); return Ok(()); },
        result = driver.startup() => result,
    };
    if let Err(error) = startup {
        if stop.is_cancelled() {
            publish(&lifecycle, Lifecycle::Stopped);
            return Ok(());
        }
        publish(&lifecycle, Lifecycle::StartupFailed(error));
        return Err(Error::Startup(error));
    }
    // Recovery owns admitted work through cancellation. Join it before releasing
    // the singleton, and never publish a catalog before boot synchronization.
    let home = driver.prepare_home(&stop).await;
    if home.is_err() || stop.is_cancelled() {
        let cleanup = driver.stop_collectors().await;
        publish(&lifecycle, Lifecycle::Stopped);
        return if stop.is_cancelled() {
            cleanup
        } else {
            home.and(cleanup)
        };
    }
    driver.start_collectors(&stop);
    let sweeper = store.map(|store| tokio::spawn(upload::sweep(store, stop.clone())));
    let result = run_loop(&config, &endpoint, &bearer, &lifecycle, &mut driver, &stop).await;
    stop.cancel();
    let collectors = driver.stop_collectors().await;
    if let Some(sweeper) = sweeper {
        sweeper.await.map_err(|_| Error::Worker)?;
    }
    publish(&lifecycle, Lifecycle::Stopped);
    result.and(collectors)
}

async fn run_loop<D: Driver>(
    config: &HomeConfig,
    endpoint: &str,
    bearer: &str,
    lifecycle: &watch::Sender<Lifecycle>,
    driver: &mut D,
    stop: &CancellationToken,
) -> Result<(), Error> {
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        publish(lifecycle, Lifecycle::Dialing);
        let dial = tokio::select! {
            biased;
            _ = stop.cancelled() => { return Ok(()); },
            result = driver.dial(endpoint, bearer, stop) => result,
        };
        match dial {
            Ok(connection) => {
                publish(lifecycle, Lifecycle::Connected);
                // Never discard this future on cancellation: run_connected
                // joins its private peer, command and transport owners.
                let result = driver.serve(connection, config.clone(), stop.clone()).await;
                if stop.is_cancelled() {
                    return Ok(());
                }
                publish(
                    lifecycle,
                    match result {
                        Ok(()) => Lifecycle::PeerEnded,
                        Err(error) => Lifecycle::PeerFailed(error),
                    },
                );
            }
            Err(_) if stop.is_cancelled() => {
                return Ok(());
            }
            Err(error) => publish(lifecycle, Lifecycle::DialFailed(error)),
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => { return Ok(()); },
            _ = sleep(RETRY_DELAY) => {},
        }
    }
}

#[cfg(test)]
#[path = "connector_tests.rs"]
mod tests;
