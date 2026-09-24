use hmux_core::command::{CommandRunner, CommandSpec, RunErrorKind};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

fn runner() -> CommandRunner {
    CommandRunner::new(1).unwrap()
}

fn helper(mode: &str, timeout: Duration) -> CommandSpec {
    CommandSpec::new(std::env::current_exe().unwrap(), 4096, timeout)
        .args(["--exact", "subprocess_helper", "--nocapture"])
        .env("HMUX_COMMAND_HELPER", mode)
}

fn marker() -> PathBuf {
    std::env::temp_dir().join(format!(
        "hmux-command-test-{}-{}",
        std::process::id(),
        NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn wait_for_marker(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(Instant::now() < deadline, "helper did not start");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// The descendant case deliberately exits before its short-lived child so the
// runner must stop draining an inherited pipe at its deadline.
#[allow(clippy::zombie_processes)]
#[test]
fn subprocess_helper() {
    let Ok(mode) = std::env::var("HMUX_COMMAND_HELPER") else {
        return;
    };
    match mode.as_str() {
        "partial" => {
            std::io::stdout().write_all(b"record").unwrap();
            std::io::stdout().flush().unwrap();
            std::process::exit(7);
        }
        "stderr" => {
            std::io::stderr().write_all(b"secret-diagnostic").unwrap();
            std::io::stderr().flush().unwrap();
            std::process::exit(7);
        }
        "stderr-overflow" => {
            std::io::stderr().write_all(&vec![b'x'; 65_537]).unwrap();
        }
        "sleep" => {
            let path = PathBuf::from(std::env::var_os("HMUX_COMMAND_MARKER").unwrap());
            std::fs::write(&path, b"started").unwrap();
            std::thread::sleep(Duration::from_millis(600));
            std::fs::write(path.with_extension("finished"), b"finished").unwrap();
        }
        "descendant" => {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "subprocess_helper", "--nocapture"])
                .env("HMUX_COMMAND_HELPER", "hold-pipe")
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
            child.spawn().unwrap();
        }
        "hold-pipe" => std::thread::sleep(Duration::from_millis(300)),
        _ => panic!("unknown synthetic helper mode"),
    }
}

#[tokio::test]
async fn exact_limit_and_overflow() {
    let runner = runner();
    let exact = runner
        .run(CommandSpec::new("/usr/bin/printf", 5, Duration::from_secs(1)).arg("12345"))
        .await
        .unwrap();
    assert_eq!(exact.stdout, b"12345");
    assert!(!exact.partial);
    let error = runner
        .run(CommandSpec::new("/usr/bin/printf", 5, Duration::from_secs(1)).arg("123456"))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::StdoutLimit);
    assert_eq!(runner.available_slots(), 1);
}

#[tokio::test]
async fn partial_exit_only_when_explicit_and_bounded() {
    let runner = runner();
    let result = runner
        .run(helper("partial", Duration::from_secs(1)).partial_exit(7))
        .await
        .unwrap();
    assert!(result.partial);
    assert!(result.stdout.windows(6).any(|window| window == b"record"));
    let error = runner
        .run(helper("partial", Duration::from_secs(1)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::Exit);
    assert_eq!(error.exit_code(), Some(7));
    let error = runner
        .run(helper("partial", Duration::from_secs(1)).partial_exit(8))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::Exit);
    let error = runner
        .run(
            CommandSpec::new(std::env::current_exe().unwrap(), 2, Duration::from_secs(1))
                .args(["--exact", "subprocess_helper", "--nocapture"])
                .env("HMUX_COMMAND_HELPER", "partial")
                .partial_exit(7),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::StdoutLimit);
}

#[tokio::test]
async fn stderr_is_bounded_and_private_in_formatting() {
    let runner = runner();
    let spec = helper("stderr", Duration::from_secs(1))
        .arg("secret-argument")
        .env("HMUX_SECRET", "secret-value");
    assert!(!format!("{spec:?}").contains("secret"));
    let error = runner.run(spec).await.err().unwrap();
    assert_eq!(error.kind(), RunErrorKind::Exit);
    assert_eq!(error.stderr(), Some(b"secret-diagnostic".as_slice()));
    assert!(!format!("{error}").contains("secret"));
    assert!(!format!("{error:?}").contains("secret"));
    let error = runner
        .run(helper("stderr-overflow", Duration::from_secs(1)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::StderrLimit);
    assert_eq!(error.stderr(), None);
}

#[tokio::test]
async fn admission_drop_and_timeout_reap_owned_child() {
    let runner = runner();
    let start = marker();
    let spec = helper("sleep", Duration::from_secs(2)).env("HMUX_COMMAND_MARKER", &start);
    let running = tokio::spawn({
        let runner = runner.clone();
        async move { runner.run(spec).await }
    });
    wait_for_marker(&start).await;
    assert_eq!(runner.available_slots(), 0);
    let busy = runner
        .run(CommandSpec::new("/usr/bin/printf", 10, Duration::from_secs(1)).arg("x"))
        .await
        .err()
        .unwrap();
    assert_eq!(busy.kind(), RunErrorKind::Busy);
    running.abort();
    let _ = running.await;
    let deadline = Instant::now() + Duration::from_secs(2);
    while runner.available_slots() != 1 {
        assert!(Instant::now() < deadline, "dropped child did not reap");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert!(!start.with_extension("finished").exists());
    std::fs::remove_file(&start).unwrap();

    let start = marker();
    let error = runner
        .run(helper("sleep", Duration::from_millis(40)).env("HMUX_COMMAND_MARKER", &start))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::TimedOut);
    assert_eq!(runner.available_slots(), 1);
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert!(!start.with_extension("finished").exists());
    std::fs::remove_file(&start).unwrap();
}

#[tokio::test]
async fn descendant_pipe_holder_is_deadline_bounded() {
    let runner = runner();
    let started = Instant::now();
    let error = runner
        .run(helper("descendant", Duration::from_millis(80)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), RunErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(runner.available_slots(), 1);
    tokio::time::sleep(Duration::from_millis(350)).await;
}

#[tokio::test]
async fn explicit_cancellation_reaps_before_return() {
    let runner = runner();
    let start = marker();
    let (cancel, receiver) = tokio::sync::oneshot::channel();
    let spec = helper("sleep", Duration::from_secs(2)).env("HMUX_COMMAND_MARKER", &start);
    let running = tokio::spawn({
        let runner = runner.clone();
        async move { runner.run_cancelable(spec, receiver).await }
    });
    wait_for_marker(&start).await;
    cancel.send(()).unwrap();
    let error = running.await.unwrap().err().unwrap();
    assert_eq!(error.kind(), RunErrorKind::Cancelled);
    assert_eq!(runner.available_slots(), 1);
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert!(!start.with_extension("finished").exists());
    std::fs::remove_file(&start).unwrap();
}
