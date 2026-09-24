//! A portable PTY may open its master before setting CLOEXEC. Concurrent HMux
//! spawns must wait until descriptor setup finishes, rather than inherit it.
use hmux_core::command::{with_child_spawn, CommandRunner, CommandSpec};
use rustix::io::{fcntl_dupfd_cloexec, fcntl_setfd, FdFlags};
use std::{fs::File, os::fd::AsRawFd, sync::mpsc, time::Duration};

#[test]
fn child_probe() {
    let Ok(fd) = std::env::var("HMUX_SPAWN_TEST_FD") else {
        return;
    };
    let path = format!("/dev/fd/{fd}");
    assert!(
        !std::path::Path::new(&path).exists(),
        "transient parent descriptor leaked into child"
    );
}

#[test]
fn concurrent_command_cannot_inherit_descriptor_during_setup() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let setup = std::thread::spawn(move || {
        with_child_spawn(|| {
            let file = File::open("/dev/null").unwrap();
            let transient = fcntl_dupfd_cloexec(&file, 128).unwrap();
            fcntl_setfd(&transient, FdFlags::empty()).unwrap();
            ready_tx.send(transient.as_raw_fd()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            fcntl_setfd(&transient, FdFlags::CLOEXEC).unwrap();
            transient
        })
    });
    let fd = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let command = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        started_tx.send(()).unwrap();
        let result = runtime.block_on(
            CommandRunner::new(1).unwrap().run(
                CommandSpec::new(
                    std::env::current_exe().unwrap(),
                    4096,
                    Duration::from_secs(3),
                )
                .args(["--exact", "child_probe", "--nocapture"])
                .env("HMUX_SPAWN_TEST_FD", fd.to_string()),
            ),
        );
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = done_rx.recv_timeout(Duration::from_millis(100));
    release_tx.send(()).unwrap();
    let descriptor = setup.join().unwrap();
    assert!(
        matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
        "spawn did not wait for descriptor setup"
    );
    assert!(done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_ok());
    command.join().unwrap();
    drop(descriptor);
}
