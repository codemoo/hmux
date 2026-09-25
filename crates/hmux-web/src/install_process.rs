//! Owned installer children, with inherited terminal and bounded capture.
use crate::args::invalid;
use std::{io, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

pub async fn run(
    command: &mut Command,
    capture: bool,
    limit: Duration,
    stop: &CancellationToken,
) -> io::Result<Vec<u8>> {
    if stop.is_cancelled() {
        return Err(invalid("installation cancelled"));
    }
    command.stdin(Stdio::inherit()).stderr(Stdio::inherit());
    command.stdout(if capture {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    let mut child = hmux_core::command::with_child_spawn(|| command.spawn())?;
    let output = child.stdout.take();
    let read = async move {
        let mut bytes = Vec::new();
        if let Some(output) = output {
            output.take(8193).read_to_end(&mut bytes).await?;
        }
        if bytes.len() > 8192 {
            return Err(invalid("installer child output exceeds limit"));
        }
        Ok::<_, io::Error>(bytes)
    };
    tokio::pin!(read);
    let work = async {
        let (status, bytes) = tokio::try_join!(child.wait(), &mut read)?;
        if !status.success() {
            return Err(invalid(
                "installation command failed; see the message above",
            ));
        }
        Ok(bytes)
    };
    let result = tokio::select! {
        result = tokio::time::timeout(limit, work) => result.unwrap_or_else(|_|Err(invalid("installation command timed out"))),
        () = stop.cancelled() => Err(invalid("installation cancelled")),
    };
    if result.is_err() {
        if let Some(pid) = child
            .id()
            .and_then(|p| rustix::process::Pid::from_raw(p as i32))
        {
            // sudo forwards TERM; ssh closes its owned PTY. The receiving HMux
            // installer handles TERM/HUP and completes its rollback transaction.
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        if tokio::time::timeout(Duration::from_secs(60), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            eprintln!("Installer cleanup exceeded its deadline. Check the target installation before retrying.");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_stops_and_joins_the_owned_child() {
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let task = tokio::spawn(async move {
            let mut command = Command::new("/bin/sleep");
            command.arg("20");
            run(&mut command, true, Duration::from_secs(30), &stop).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
    }
}
