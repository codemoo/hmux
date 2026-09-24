use super::*;
use futures_util::StreamExt;
use hmux_protocol::protobuf::{self, types as p, Direction, Negotiated};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant as StdInstant,
};
use tokio::io::DuplexStream;
use tokio::{
    sync::{oneshot, Notify},
    time::timeout,
};
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-connector-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn config(&self) -> HomeConfig {
        HomeConfig {
            schema_version: SCHEMA_VERSION,
            role: "home".into(),
            inventory_path: self.0.join("inventory.toml"),
            state_dir: self.0.join("state"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Probe {
    preparing: AtomicBool,
    collectors_started: AtomicUsize,
    collectors_stopped: AtomicUsize,
    startup: AtomicUsize,
    attempts: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    peers: AtomicUsize,
    cleanup_started: AtomicBool,
    times: Mutex<Vec<StdInstant>>,
    release_cleanup: Notify,
}
#[derive(Clone, Copy)]
enum Startup {
    Ready,
    Fail,
    Wait,
}
#[derive(Clone, Copy)]
enum Dial {
    Fail,
    FailThenConnect,
    Connect,
    Wait,
}
#[derive(Clone, Copy)]
enum Serve {
    ImmediateFailure,
    UntilStopped,
    CleanupGate,
}
struct Fake {
    prepare_gate: bool,
    probe: Arc<Probe>,
    startup: Startup,
    dial: Dial,
    serve: Serve,
}
impl Fake {
    fn new(probe: Arc<Probe>, dial: Dial, serve: Serve) -> Self {
        Self {
            prepare_gate: false,
            probe,
            startup: Startup::Ready,
            dial,
            serve,
        }
    }
}
struct Active(Arc<Probe>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Driver for Fake {
    async fn prepare_home(&mut self, stop: &CancellationToken) -> Result<(), Error> {
        if self.prepare_gate {
            self.probe.preparing.store(true, Ordering::SeqCst);
            stop.cancelled().await;
            self.probe.cleanup_started.store(true, Ordering::SeqCst);
            self.probe.release_cleanup.notified().await;
            return Err(Error::Recovery);
        }
        Ok(())
    }
    fn start_collectors(&mut self, _stop: &CancellationToken) {
        self.probe.collectors_started.fetch_add(1, Ordering::SeqCst);
    }
    async fn stop_collectors(&mut self) -> Result<(), Error> {
        self.probe.collectors_stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    type Connected = ();
    async fn startup(&mut self) -> Result<(), dial::Error> {
        self.probe.startup.fetch_add(1, Ordering::SeqCst);
        match self.startup {
            Startup::Ready => Ok(()),
            Startup::Fail => Err(dial::Error::Tls),
            Startup::Wait => std::future::pending().await,
        }
    }
    async fn dial(
        &mut self,
        _endpoint: &str,
        _bearer: &str,
        _stop: &CancellationToken,
    ) -> Result<Self::Connected, dial::Error> {
        let number = self.probe.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        let active = self.probe.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.peak.fetch_max(active, Ordering::SeqCst);
        self.probe.times.lock().unwrap().push(StdInstant::now());
        let _active = Active(self.probe.clone());
        match self.dial {
            Dial::Fail => Err(dial::Error::Connect),
            Dial::FailThenConnect if number == 1 => Err(dial::Error::Tls),
            Dial::FailThenConnect | Dial::Connect => Ok(()),
            Dial::Wait => std::future::pending().await,
        }
    }
    async fn serve(
        &mut self,
        _connection: Self::Connected,
        _config: HomeConfig,
        stop: CancellationToken,
    ) -> Result<(), peer::Error> {
        self.probe.peers.fetch_add(1, Ordering::SeqCst);
        match self.serve {
            Serve::ImmediateFailure => Err(peer::Error::Transport),
            Serve::UntilStopped => {
                stop.cancelled().await;
                Ok(())
            }
            Serve::CleanupGate => {
                stop.cancelled().await;
                self.probe.cleanup_started.store(true, Ordering::SeqCst);
                self.probe.release_cleanup.notified().await;
                Ok(())
            }
        }
    }
}
async fn wait_for(probe: &Probe, count: usize) {
    timeout(Duration::from_secs(5), async {
        while probe.attempts.load(Ordering::SeqCst) < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn state(receiver: &mut LifecycleObserver, target: Lifecycle) {
    timeout(Duration::from_secs(2), async {
        loop {
            if receiver.latest() == target {
                return;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[test]
fn prepare_validates_before_network_or_lock_and_drop_is_inert() {
    let fixture = Fixture::new();
    let mut wrong = fixture.config();
    wrong.role = "gateway".into();
    let mut relative = fixture.config();
    relative.inventory_path = PathBuf::from("inventory.toml");
    assert!(matches!(
        Prepared::prepare(relative, "wss://gateway.test/connect", "synthetic"),
        Err(Error::Config)
    ));
    assert!(matches!(
        Prepared::prepare(wrong, "wss://gateway.test/connect", "synthetic"),
        Err(Error::Config)
    ));
    assert!(matches!(
        Prepared::prepare(
            fixture.config(),
            "https://gateway.test/connect",
            "synthetic"
        ),
        Err(Error::Endpoint)
    ));
    assert!(matches!(
        Prepared::prepare(
            fixture.config(),
            "wss://gateway.test/connect",
            "bad\r\ntoken"
        ),
        Err(Error::Bearer)
    ));
    assert!(!fixture.0.join("state").exists());
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    assert_eq!(format!("{prepared:?}"), "Prepared([redacted])");
    assert!(matches!(
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic"),
        Err(Error::Lock(singleton::Error::AlreadyRunning))
    ));
    drop(prepared);
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}

#[tokio::test]
async fn unpolled_run_drops_lock_without_startup_or_dial() {
    let fixture = Fixture::new();
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    let probe = Arc::new(Probe::default());
    let future = prepared.run_with(
        Fake::new(probe.clone(), Dial::Connect, Serve::UntilStopped),
        CancellationToken::new(),
    );
    drop(future);
    assert_eq!(probe.startup.load(Ordering::SeqCst), 0);
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}

#[tokio::test]
async fn retry_delay_is_three_seconds_and_attempts_are_sequential() {
    let fixture = Fixture::new();
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    let mut status = prepared.lifecycle();
    let probe = Arc::new(Probe::default());
    let stop = CancellationToken::new();
    let owner = tokio::spawn(prepared.run_with(
        Fake::new(probe.clone(), Dial::FailThenConnect, Serve::UntilStopped),
        stop.clone(),
    ));
    state(&mut status, Lifecycle::DialFailed(dial::Error::Tls)).await;
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    wait_for(&probe, 2).await;
    state(&mut status, Lifecycle::Connected).await;
    let elapsed = {
        let times = probe.times.lock().unwrap();
        times[1].duration_since(times[0])
    };
    assert!(elapsed >= RETRY_DELAY);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 1);
    assert_eq!(probe.peers.load(Ordering::SeqCst), 1);
    assert!(matches!(
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic"),
        Err(Error::Lock(singleton::Error::AlreadyRunning))
    ));
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert_eq!(status.latest(), Lifecycle::Stopped);
    assert_eq!(probe.collectors_started.load(Ordering::SeqCst), 1);
    assert_eq!(probe.collectors_stopped.load(Ordering::SeqCst), 1);
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}

#[tokio::test]
async fn shutdown_during_sleep_dial_and_root_startup_releases_owner() {
    for phase in ["sleep", "dial", "startup"] {
        let fixture = Fixture::new();
        let prepared =
            Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
        let mut status = prepared.lifecycle();
        let probe = Arc::new(Probe::default());
        let fake = Fake {
            startup: if phase == "startup" {
                Startup::Wait
            } else {
                Startup::Ready
            },
            ..Fake::new(
                probe.clone(),
                if phase == "dial" {
                    Dial::Wait
                } else {
                    Dial::Fail
                },
                Serve::ImmediateFailure,
            )
        };
        let stop = CancellationToken::new();
        let owner = tokio::spawn(prepared.run_with(fake, stop.clone()));
        timeout(Duration::from_secs(2), async {
            while probe.startup.load(Ordering::SeqCst) == 0
                || phase != "startup" && probe.attempts.load(Ordering::SeqCst) == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if phase == "sleep" {
            state(&mut status, Lifecycle::DialFailed(dial::Error::Connect)).await;
        }
        stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(2), owner)
                .await
                .unwrap()
                .unwrap(),
            Ok(()),
            "{phase}"
        );
        assert_eq!(probe.active.load(Ordering::SeqCst), 0);
        drop(
            Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap(),
        );
    }
}

#[tokio::test]
async fn interrupted_recovery_joins_before_singleton_release_or_catalog_start() {
    for abort in [false, true] {
        let fixture = Fixture::new();
        let prepared =
            Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
        let probe = Arc::new(Probe::default());
        let stop = CancellationToken::new();
        let fake = Fake {
            prepare_gate: true,
            ..Fake::new(probe.clone(), Dial::Connect, Serve::UntilStopped)
        };
        let owner = tokio::spawn(prepared.run_with(fake, stop.clone()));
        timeout(Duration::from_secs(2), async {
            while !probe.preparing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if abort {
            owner.abort();
        } else {
            stop.cancel();
        }
        timeout(Duration::from_secs(2), async {
            while !probe.cleanup_started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic"),
            Err(Error::Lock(singleton::Error::AlreadyRunning))
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
        assert_eq!(probe.collectors_started.load(Ordering::SeqCst), 0);
        probe.release_cleanup.notify_one();
        if abort {
            assert!(owner.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(owner.await.unwrap(), Ok(()));
        }
        timeout(Duration::from_secs(2), async {
            loop {
                match Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic")
                {
                    Ok(owner) => {
                        drop(owner);
                        break;
                    }
                    Err(Error::Lock(singleton::Error::AlreadyRunning)) => {
                        tokio::task::yield_now().await
                    }
                    other => panic!("unexpected owner: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(probe.collectors_stopped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn caller_abort_keeps_singleton_through_private_peer_cleanup() {
    let fixture = Fixture::new();
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    let mut status = prepared.lifecycle();
    let probe = Arc::new(Probe::default());
    let owner = tokio::spawn(prepared.run_with(
        Fake::new(probe.clone(), Dial::Connect, Serve::CleanupGate),
        CancellationToken::new(),
    ));
    state(&mut status, Lifecycle::Connected).await;
    owner.abort();
    assert!(owner.await.is_err());
    timeout(Duration::from_secs(2), async {
        while !probe.cleanup_started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic"),
        Err(Error::Lock(singleton::Error::AlreadyRunning))
    ));
    probe.release_cleanup.notify_one();
    timeout(Duration::from_secs(2), async {
        loop {
            match Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic") {
                Ok(owner) => {
                    drop(owner);
                    break;
                }
                Err(Error::Lock(singleton::Error::AlreadyRunning)) => {
                    tokio::task::yield_now().await
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status.latest(), Lifecycle::Stopped);
}

#[tokio::test]
async fn startup_failure_is_fixed_and_does_not_dial() {
    let fixture = Fixture::new();
    let prepared = Prepared::prepare(
        fixture.config(),
        "wss://gateway.test/connect",
        "private-token",
    )
    .unwrap();
    let status = prepared.lifecycle();
    let probe = Arc::new(Probe::default());
    let fake = Fake {
        startup: Startup::Fail,
        ..Fake::new(probe.clone(), Dial::Connect, Serve::UntilStopped)
    };
    assert_eq!(
        prepared.run_with(fake, CancellationToken::new()).await,
        Err(Error::Startup(dial::Error::Tls))
    );
    assert_eq!(status.latest(), Lifecycle::StartupFailed(dial::Error::Tls));
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}

struct InMemoryPeer {
    gateway: Option<oneshot::Sender<WebSocketStream<DuplexStream>>>,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
}
impl Driver for InMemoryPeer {
    type Connected = transport::Connection;
    async fn startup(&mut self) -> Result<(), dial::Error> {
        Ok(())
    }
    async fn dial(
        &mut self,
        _endpoint: &str,
        _bearer: &str,
        _stop: &CancellationToken,
    ) -> Result<Self::Connected, dial::Error> {
        let (left, right) = tokio::io::duplex(64 * 1024);
        let home =
            WebSocketStream::from_raw_socket(left, Role::Client, Some(transport::socket_config()))
                .await;
        let gateway =
            WebSocketStream::from_raw_socket(right, Role::Server, Some(transport::socket_config()))
                .await;
        self.gateway.take().unwrap().send(gateway).ok();
        transport::start(home, Negotiated::ProtobufV2, Direction::ToHome)
            .map_err(|_| dial::Error::Unavailable)
    }
    async fn serve(
        &mut self,
        connection: Self::Connected,
        config: HomeConfig,
        stop: CancellationToken,
    ) -> Result<(), peer::Error> {
        peer::run_connected(
            connection,
            config,
            self.catalog.clone(),
            self.runner.clone(),
            stop,
        )
        .await
    }
}

#[tokio::test]
async fn connected_path_runs_current_read_only_peer_and_joins_before_unlock() {
    let fixture = Fixture::new();
    let script = fixture.0.join("fake-tmux");
    fs::write(
        &script,
        "#!/bin/sh\nsleep 0.2\nprintf 'no server running on /synthetic\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let reader = TmuxCatalogReader::new(script, None, Duration::from_secs(3)).unwrap();
    let (gateway_tx, gateway_rx) = oneshot::channel();
    let driver = InMemoryPeer {
        gateway: Some(gateway_tx),
        catalog: reader,
        runner: CommandRunner::new(1).unwrap(),
    };
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    let mut status = prepared.lifecycle();
    let stop = CancellationToken::new();
    let owner = tokio::spawn(prepared.run_with(driver, stop.clone()));
    let mut gateway = timeout(Duration::from_secs(2), gateway_rx)
        .await
        .unwrap()
        .unwrap();
    state(&mut status, Lifecycle::Connected).await;
    let frame = timeout(Duration::from_secs(2), gateway.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let hello = protobuf::decode(frame.into_data(), Direction::ToGateway).unwrap();
    assert!(
        matches!(hello.body, Some(p::envelope::Body::Hello(p::Hello { capabilities })) if capabilities == [hmux_protocol::flow::CAPABILITY])
    );
    let frame = timeout(Duration::from_secs(2), gateway.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let catalog = protobuf::decode(frame.into_data(), Direction::ToGateway).unwrap();
    assert!(matches!(catalog.body, Some(p::envelope::Body::Catalog(_))));
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert_eq!(status.latest(), Lifecycle::Stopped);
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}

#[tokio::test]
async fn peer_failure_is_categorized_and_retries_after_delay() {
    let fixture = Fixture::new();
    let prepared =
        Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap();
    let mut status = prepared.lifecycle();
    let probe = Arc::new(Probe::default());
    let stop = CancellationToken::new();
    let owner = tokio::spawn(prepared.run_with(
        Fake::new(probe.clone(), Dial::Connect, Serve::ImmediateFailure),
        stop.clone(),
    ));
    state(&mut status, Lifecycle::PeerFailed(peer::Error::Transport)).await;
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    wait_for(&probe, 2).await;
    let elapsed = {
        let times = probe.times.lock().unwrap();
        times[1].duration_since(times[0])
    };
    assert!(elapsed >= RETRY_DELAY);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 1);
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn upload_sweeper_runs_during_dial_and_releases_lock_after_cancellation() {
    use rustix::fs::{flock, FlockOperation};
    let fixture = Fixture::new();
    let root = fixture.0.join("hmux/staged-files-v1");
    let store = Arc::new(Store::open(root.clone()).unwrap());
    // A recognized expired stage from an earlier process, without user data.
    let expired = root.join("1700000000-00000000000000000000000000000000");
    fs::DirBuilder::new().mode(0o700).create(&expired).unwrap();
    let prepared = Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic")
        .unwrap()
        .with_upload_store(store.clone());
    let probe = Arc::new(Probe::default());
    let stop = CancellationToken::new();
    let owner = tokio::spawn(prepared.run_with(
        Fake::new(probe.clone(), Dial::Wait, Serve::UntilStopped),
        stop.clone(),
    ));
    wait_for(&probe, 1).await;
    timeout(Duration::from_secs(3), async {
        while expired.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(root.join(".lock"))
        .unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    // A second connector with its startup sweep blocked on another process's
    // lock must still cancel and join rather than outlive the singleton.
    let prepared = Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic")
        .unwrap()
        .with_upload_store(store);
    let stop = CancellationToken::new();
    let owner = tokio::spawn(prepared.run_with(
        Fake::new(Arc::new(Probe::default()), Dial::Wait, Serve::UntilStopped),
        stop.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    drop(Prepared::prepare(fixture.config(), "wss://gateway.test/connect", "synthetic").unwrap());
}
