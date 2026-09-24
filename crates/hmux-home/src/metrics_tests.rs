use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-metrics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn script(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn collector(&self) -> Arc<Collector> {
        let top = self.script("top", "printf 'CPU usage: 2%% user, 3%% sys, 95%% idle\\nCPU usage: 10%% user, 15%% sys, 75%% idle\\n'");
        let vm = self.script("vm", "printf 'Mach Virtual Memory Statistics: (page size of 4096 bytes)\\nAnonymous pages: 100.\\nPages wired down: 20.\\nPages purgeable: 10.\\nPages occupied by compressor: 5.\\n'");
        let sysctl = self.script("sysctl", "printf '1048576\\n'");
        let ioreg = self.script("ioreg", "if [ \"$4\" = IOAccelerator ]; then printf '<plist><dict/></plist>'; else printf '<plist><dict><key>GPU Activity(%%)</key><real>42.25</real></dict></plist>'; fi");
        Arc::new(Collector {
            source: Source::Darwin {
                top,
                vm,
                sysctl,
                ioreg,
            },
            disk: false,
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn synthetic_darwin_samples_and_replaces_failed_fields() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let collector = f.collector();
    let stop = CancellationToken::new();
    let value = collector.clone().sample(&stop).await.unwrap();
    assert_eq!(value.cpu_percent, Some(25.0));
    assert_eq!(value.gpu_percent, Some(42.25));
    assert_eq!(value.memory_used_bytes, Some(115 * 4096));
    assert_eq!(value.memory_total_bytes, Some(1048576));
    assert_eq!(value.disk_total_bytes, None);
    assert!(value.validate().is_ok());
    f.script("top", "exit 1");
    f.script("ioreg", "exit 1");
    let value = collector.clone().sample(&stop).await.unwrap();
    assert_eq!(value.cpu_percent, None);
    assert_eq!(value.gpu_percent, None);
    assert!(value.memory_total_bytes.is_some());
    f.script("vm", "exit 1");
    assert!(collector.sample(&stop).await.is_none());
}

#[tokio::test]
async fn clean_environment_and_literal_arguments_are_used() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let path=f.script("check", "[ \"$LANG\" = C ] && [ \"$LC_ALL\" = C ] && [ \"$PATH\" = /usr/bin:/bin:/usr/sbin:/sbin ] && [ -z \"${HOME+x}\" ] && [ \"$1\" = 'a ; $(no-execution)' ] || exit 42\nprintf ok");
    let value = command(
        &path,
        &["a ; $(no-execution)"],
        64,
        &CancellationToken::new(),
        Instant::now() + TIMEOUT,
    )
    .await;
    assert_eq!(value.as_deref(), Some(&b"ok"[..]));
}

#[tokio::test]
async fn timeout_omits_only_failed_lanes_and_reaps_the_child() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let collector = f.collector();
    f.script(
        "top",
        "root=${0%/*}; printf '%s\\n' \"$$\" > \"$root/pid\"; exec /bin/sleep 20",
    );
    let started = Instant::now();
    let result = collector.sample(&CancellationToken::new()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(result.cpu_percent, None);
    assert!(result.memory_total_bytes.is_some());
    let pid = fs::read_to_string(f.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
}

#[tokio::test]
async fn cancellation_and_single_process_wide_admission_are_joined() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let collector = f.collector();
    f.script(
        "top",
        "root=${0%/*}; printf '%s\\n' \"$$\" > \"$root/pid\"; exec /bin/sleep 20",
    );
    let stop = CancellationToken::new();
    let owned = stop.clone();
    let source = collector.clone();
    let worker = tokio::spawn(async move { source.sample(&owned).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !f.0.join("pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let started = Instant::now();
    assert!(collector.sample(&CancellationToken::new()).await.is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    stop.cancel();
    assert!(worker.await.unwrap().is_none());
    let pid = fs::read_to_string(f.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
    assert_eq!(SLOTS.get().unwrap().available_permits(), 1);
}

#[tokio::test]
async fn synthetic_linux_delta_and_oversized_proc_input() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let stat = f.0.join("stat");
    let memory = f.0.join("meminfo");
    fs::write(&stat, "cpu 100 0 100 800 0 0 0 0 0 0\n").unwrap();
    fs::write(&memory, "MemTotal: 1000 kB\nMemAvailable: 400 kB\n").unwrap();
    let collector = Arc::new(Collector {
        source: Source::Linux {
            stat: stat.clone(),
            memory: memory.clone(),
        },
        disk: false,
    });
    let source = collector.clone();
    let worker = tokio::spawn(async move { source.sample(&CancellationToken::new()).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    fs::write(&stat, "cpu 150 0 100 850 0 0 0 0 0 0\n").unwrap();
    let value = worker.await.unwrap().unwrap();
    assert_eq!(value.cpu_percent, Some(50.0));
    assert_eq!(value.memory_used_bytes, Some(600 * 1024));
    fs::write(&stat, vec![b'x'; TEXT_MAX + 1]).unwrap();
    fs::write(&memory, vec![b'x'; TEXT_MAX + 1]).unwrap();
    assert!(collector.sample(&CancellationToken::new()).await.is_none());
}

#[tokio::test]
async fn latest_sample_is_cleared_when_all_collectors_fail() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let collector = f.collector();
    let stop = CancellationToken::new();
    let (tx, mut rx) = watch::channel(None);
    let worker = tokio::spawn(run(collector, tx, stop.clone()));
    tokio::time::timeout(Duration::from_secs(2), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(rx.borrow_and_update().is_some());
    for name in ["top", "vm", "ioreg"] {
        f.script(name, "exit 1");
    }
    tokio::time::timeout(Duration::from_secs(7), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(rx.borrow_and_update().is_none());
    stop.cancel();
    worker.await.unwrap();
}

#[test]
fn many_cpu_rows_do_not_hide_bounded_aggregate_line() {
    let f = Fixture::new();
    let stat = f.0.join("stat");
    let head = "cpu 100 0 100 800 0 0 0 0 0 0\n";
    fs::write(
        &stat,
        format!("{head}{}", "cpu0 100 0 100 800\n".repeat(8192)),
    )
    .unwrap();
    assert_eq!(read_cpu(&stat).unwrap(), head.as_bytes());
    fs::write(&stat, vec![b'x'; TEXT_MAX + 1]).unwrap();
    assert!(read_cpu(&stat).is_none());
}

#[tokio::test]
async fn both_wire_codecs_publish_basic_catalog_before_slow_metrics() {
    use crate::{
        catalog::TmuxCatalogReader,
        config::HomeConfig,
        peer::{self, Services},
    };
    use futures_util::StreamExt;
    use hmux_protocol::{
        legacy,
        protobuf::{self as pb, types as p, Direction, Negotiated},
        transport, wire,
    };
    use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let collector = f.collector();
        f.script(
            "top",
            "root=${0%/*}; printf '%s\\n' \"$$\" > \"$root/pid\"; exec /bin/sleep 20",
        );
        let tmux=f.script("tmux","case \"$1\" in\nlist-sessions) printf '%s\\n' '$7|:hmux-sep-v1:|synthetic|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;;\nlist-windows) : ;;\n*) exit 99 ;;\nesac");
        let reader = TmuxCatalogReader::new(tmux, None, Duration::from_secs(2)).unwrap();
        let config = HomeConfig {
            schema_version: 1,
            role: "home".into(),
            state_dir: f.0.clone(),
            inventory_path: f.0.join("unused"),
        };
        let (left, right) = tokio::io::duplex(64 << 10);
        let home =
            WebSocketStream::from_raw_socket(left, Role::Client, Some(transport::socket_config()))
                .await;
        let mut gateway =
            WebSocketStream::from_raw_socket(right, Role::Server, Some(transport::socket_config()))
                .await;
        let connection = transport::start(home, protocol, Direction::ToHome).unwrap();
        let stop = CancellationToken::new();
        let owner = tokio::spawn(peer::run_connected_with_services(
            connection,
            config,
            reader,
            CommandRunner::new(2).unwrap(),
            Services {
                metrics: Some(collector),
                ..Services::default()
            },
            stop.clone(),
        ));
        let mut catalog_count = 0;
        let started = Instant::now();
        tokio::time::timeout(Duration::from_secs(7), async {
            while let Some(frame) = gateway.next().await {
                let raw = frame.unwrap().into_data();
                let envelope = match protocol {
                    Negotiated::JsonV1 => legacy::from_json(
                        wire::Message::decode(&raw).unwrap(),
                        Direction::ToGateway,
                    )
                    .unwrap(),
                    Negotiated::ProtobufV2 => pb::decode(raw, Direction::ToGateway).unwrap(),
                };
                match envelope.body.unwrap() {
                    p::envelope::Body::Hello(_) => {}
                    p::envelope::Body::Catalog(snapshot) => {
                        catalog_count += 1;
                        let value: hmux_model::Catalog =
                            hmux_protocol::snapshots::catalog_from_proto(*snapshot).unwrap();
                        if catalog_count == 1 {
                            assert!(started.elapsed() < Duration::from_secs(2));
                            assert!(value.host_metrics.is_none());
                        } else if let Some(metrics) = value.host_metrics {
                            assert_eq!(metrics.cpu_percent, None);
                            assert_eq!(metrics.memory_used_bytes, Some(115 * 4096));
                            assert!(metrics.validate().is_ok());
                            break;
                        }
                    }
                    _ => panic!("unexpected metrics peer message"),
                }
            }
        })
        .await
        .unwrap();
        assert!(catalog_count >= 2);
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let pid = fs::read_to_string(f.0.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}
