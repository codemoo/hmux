//! Redraw only the foreground process group of this owned view's attached client.
//! Every externally supplied process or TTY identity is checked twice against
//! tmux, and the caller cannot supply a signal target.
use crate::view::{self, OwnedView, Target};
use hmux_core::command::{CommandRunner, CommandSpec};
use rustix::process::{kill_process_group, Pid, Signal};
use std::{path::Path, time::Duration};
use tokio::{sync::oneshot, time::Instant};
use tokio_util::sync::CancellationToken;

const TOTAL_TIMEOUT: Duration = Duration::from_secs(2);
const PS_OUTPUT_LIMIT: usize = 512;
const CLIENT_FORMAT: &str = "#{client_pid} #{client_tty} #{pane_id} #{pane_tty} #{pane_pid} #{session_name} #{HMUX_VIEW_OWNER} #{@hmux_app_view}";

/// Fixed categories never expose tmux output, a TTY path, or process arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Closed,
    Command,
    Unavailable,
    Changed,
    Signal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientRow {
    client_pid: i32,
    client_tty: String,
    pane_id: String,
    pane_tty: String,
    pane_pid: i32,
    session_name: String,
    owner: String,
}

fn valid_pid(raw: &str) -> Option<i32> {
    raw.parse::<i32>().ok().filter(|pid| *pid > 1)
}

fn valid_tty(raw: &str) -> bool {
    let Some(path) = raw.strip_prefix("/dev/") else {
        return false;
    };
    !path.is_empty()
        && raw.len() <= 128
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'/')
}

fn valid_pane(raw: &str) -> bool {
    raw.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && digits.parse::<u64>().is_ok()
    })
}

fn client_row(raw: &[u8], pid: i32, name: &str, nonce: &str) -> Result<ClientRow, Error> {
    let raw = std::str::from_utf8(raw).map_err(|_| Error::Unavailable)?;
    let mut found = None;
    for line in raw.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.first().and_then(|value| valid_pid(value)) != Some(pid) {
            continue;
        }
        if fields.len() != 8
            || !valid_tty(fields[1])
            || !valid_pane(fields[2])
            || !valid_tty(fields[3])
            || fields[5] != name
            || fields[6] != nonce
            || fields[7] != "1"
        {
            return Err(Error::Unavailable);
        }
        let pane_pid = valid_pid(fields[4]).ok_or(Error::Unavailable)?;
        if found.is_some() {
            return Err(Error::Unavailable);
        }
        found = Some(ClientRow {
            client_pid: pid,
            client_tty: fields[1].into(),
            pane_id: fields[2].into(),
            pane_tty: fields[3].into(),
            pane_pid,
            session_name: fields[5].into(),
            owner: fields[6].into(),
        });
    }
    found.ok_or(Error::Unavailable)
}

fn foreground_group(raw: &[u8], row: &ClientRow) -> Result<i32, Error> {
    let raw = std::str::from_utf8(raw).map_err(|_| Error::Unavailable)?;
    let fields: Vec<_> = raw.split_ascii_whitespace().collect();
    if fields.len() != 3 || valid_pid(fields[0]) != Some(row.pane_pid) {
        return Err(Error::Unavailable);
    }
    let group = valid_pid(fields[1]).ok_or(Error::Unavailable)?;
    let expected = row
        .pane_tty
        .strip_prefix("/dev/")
        .ok_or(Error::Unavailable)?;
    if fields[2] != expected && fields[2] != expected.strip_prefix("tty").unwrap_or(expected) {
        return Err(Error::Unavailable);
    }
    Ok(group)
}

fn check_cancelled(closing: &CancellationToken, stop: &CancellationToken) -> Result<(), Error> {
    if closing.is_cancelled() {
        Err(Error::Closed)
    } else if stop.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

async fn tmux_command(
    target: &Target,
    runner: &CommandRunner,
    args: Vec<String>,
    closing: &CancellationToken,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    check_cancelled(closing, stop)?;
    let cancel = CancellationToken::new();
    let work = target.command(runner, args, &cancel, deadline);
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = closing.cancelled() => {
            cancel.cancel();
            let _ = work.await;
            Err(Error::Closed)
        }
        _ = stop.cancelled() => {
            cancel.cancel();
            let _ = work.await;
            Err(Error::Cancelled)
        }
        result = &mut work => result.map_err(|error| match error {
            view::Error::Cancelled => Error::Cancelled,
            _ => Error::Command,
        }),
    }
}

async fn ps_command(
    runner: &CommandRunner,
    program: &Path,
    pane_pid: i32,
    closing: &CancellationToken,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    check_cancelled(closing, stop)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::Command);
    }
    let spec = CommandSpec::new(program, PS_OUTPUT_LIMIT, remaining).args([
        "-p",
        &pane_pid.to_string(),
        "-o",
        "pid=,tpgid=,tty=",
    ]);
    let (sender, receiver) = oneshot::channel();
    let work = runner.run_cancelable(spec, receiver);
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = closing.cancelled() => {
            drop(sender);
            let _ = work.await;
            Err(Error::Closed)
        }
        _ = stop.cancelled() => {
            drop(sender);
            let _ = work.await;
            Err(Error::Cancelled)
        }
        result = &mut work => result.map(|output| output.stdout).map_err(|_| Error::Command),
    }
}

// The private seam keeps the process lookup and signal operation injectable in
// tests while production derives every other field from an OwnedView.
#[allow(clippy::too_many_arguments)]
async fn run_with<S>(
    target: &Target,
    name: &str,
    nonce: &str,
    closing: &CancellationToken,
    runner: &CommandRunner,
    child_pid: u32,
    stop: &CancellationToken,
    ps: &Path,
    signal: S,
) -> Result<(), Error>
where
    S: Fn(i32) -> Result<(), ()>,
{
    let child_pid = i32::try_from(child_pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or(Error::Invalid)?;
    check_cancelled(closing, stop)?;
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let list = || {
        vec![
            "list-clients".into(),
            "-t".into(),
            name.into(),
            "-F".into(),
            CLIENT_FORMAT.into(),
        ]
    };
    let before = client_row(
        &tmux_command(target, runner, list(), closing, stop, deadline).await?,
        child_pid,
        name,
        nonce,
    )?;
    let group = foreground_group(
        &ps_command(runner, ps, before.pane_pid, closing, stop, deadline).await?,
        &before,
    )?;
    let after = client_row(
        &tmux_command(target, runner, list(), closing, stop, deadline).await?,
        child_pid,
        name,
        nonce,
    )?;
    if before != after {
        return Err(Error::Changed);
    }
    check_cancelled(closing, stop)?;
    if Instant::now() >= deadline {
        return Err(Error::Command);
    }
    signal(group).map_err(|_| Error::Signal)?;
    check_cancelled(closing, stop)?;
    tmux_command(
        target,
        runner,
        vec!["refresh-client".into(), "-t".into(), before.client_tty],
        closing,
        stop,
        deadline,
    )
    .await?;
    Ok(())
}

/// Refresh the owned attached view only. The supplied PID must be the process
/// ID of its directly owned tmux attach child, never a caller-selected pane PID.
pub async fn run(
    view: &OwnedView,
    runner: &CommandRunner,
    child_pid: u32,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let (target, name, nonce) = view.refresh_destination();
    run_with(
        target,
        name,
        nonce,
        view.closing_token(),
        runner,
        child_pid,
        stop,
        Path::new("/bin/ps"),
        |group| {
            let pid = Pid::from_raw(group).ok_or(())?;
            kill_process_group(pid, Signal::WINCH).map_err(|_| ())
        },
    )
    .await
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
