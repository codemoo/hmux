//! One bounded asynchronous host sampler per Home stream. Sampling never gates
//! first catalog readiness and each result replaces all fields, including failures.
use crate::metrics_parsers as parse;
#[cfg(any(target_os = "macos", test))]
use hmux_core::command::{CommandRunner, CommandSpec};
use hmux_model::HostMetrics;
#[cfg(any(target_os = "linux", test))]
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(3);
const INTERVAL: Duration = Duration::from_secs(5);
const TEXT_MAX: usize = 64 << 10;
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
#[cfg(any(target_os = "macos", test))]
static COMMANDS: OnceLock<CommandRunner> = OnceLock::new();

#[derive(Clone)]
enum Source {
    #[cfg(any(target_os = "macos", test))]
    Darwin {
        top: PathBuf,
        vm: PathBuf,
        sysctl: PathBuf,
        ioreg: PathBuf,
    },
    #[cfg(any(target_os = "linux", test))]
    Linux { stat: PathBuf, memory: PathBuf },
}
/// Explicit native collector; library fixtures leave this service disabled.
pub struct Collector {
    source: Source,
    disk: bool,
}
impl std::fmt::Debug for Collector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Collector([redacted])")
    }
}
impl Collector {
    pub fn native() -> Self {
        #[cfg(target_os = "macos")]
        let source = Source::Darwin {
            top: "/usr/bin/top".into(),
            vm: "/usr/bin/vm_stat".into(),
            sysctl: "/usr/sbin/sysctl".into(),
            ioreg: "/usr/sbin/ioreg".into(),
        };
        #[cfg(target_os = "linux")]
        let source = Source::Linux {
            stat: "/proc/stat".into(),
            memory: "/proc/meminfo".into(),
        };
        Self { source, disk: true }
    }
    async fn sample(self: Arc<Self>, stop: &CancellationToken) -> Option<HostMetrics> {
        let permit = SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .ok()?;
        let child = stop.child_token();
        let _cancel = child.clone().drop_guard();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let deadline = Instant::now() + TIMEOUT;
            let mut result = runtime.block_on(self.measure(&child, deadline));
            if child.is_cancelled() {
                return None;
            }
            if self.disk && Instant::now() < deadline {
                if let Some((used, total)) = disk() {
                    result.disk_used_bytes = Some(used);
                    result.disk_total_bytes = Some(total);
                }
            }
            if child.is_cancelled() {
                return None;
            }
            result.observed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|t| {
                    chrono::DateTime::from_timestamp(t.as_secs() as i64, t.subsec_nanos())
                })?
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
            result.validate().ok().map(|()| result)
        })
        .await
        .ok()
        .flatten();
        if stop.is_cancelled() {
            None
        } else {
            result
        }
    }
    async fn measure(&self, stop: &CancellationToken, deadline: Instant) -> HostMetrics {
        let mut result = HostMetrics::default();
        match &self.source {
            #[cfg(any(target_os = "macos", test))]
            Source::Darwin {
                top,
                vm,
                sysctl,
                ioreg,
            } => {
                let cpu = async {
                    let raw = command(
                        top,
                        &["-l", "2", "-s", "1", "-n", "0", "-stats", "pid"],
                        TEXT_MAX,
                        stop,
                        deadline,
                    )
                    .await?;
                    parse::cpu_darwin(&raw)
                };
                let memory = async {
                    let raw = command(vm, &[], TEXT_MAX, stop, deadline).await?;
                    let total = command(sysctl, &["-n", "hw.memsize"], 256, stop, deadline).await?;
                    parse::memory_darwin(&raw, &total)
                };
                let gpu = async {
                    for class in ["IOAccelerator", "AGXAccelerator"] {
                        if let Some(raw) =
                            command(ioreg, &["-a", "-r", "-c", class], 2 << 20, stop, deadline)
                                .await
                        {
                            if let Some(value) = parse::gpu_darwin(&raw) {
                                return Some(value);
                            }
                        }
                    }
                    None
                };
                let (cpu, memory, gpu) = tokio::join!(cpu, memory, gpu);
                result.cpu_percent = cpu;
                result.gpu_percent = gpu;
                if let Some((used, total)) = memory {
                    result.memory_used_bytes = Some(used);
                    result.memory_total_bytes = Some(total);
                }
            }
            #[cfg(any(target_os = "linux", test))]
            Source::Linux { stat, memory } => {
                if let Some(first) = read_cpu(stat) {
                    tokio::select! {
                        _ = stop.cancelled() => return result,
                        _ = tokio::time::sleep_until(deadline.into()) => return result,
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                    }
                    if let Some(second) = read_cpu(stat) {
                        result.cpu_percent = parse::cpu_linux(&first, &second);
                    }
                }
                if stop.is_cancelled() || Instant::now() >= deadline {
                    return result;
                }
                if let Some((used, total)) = read(memory).and_then(|raw| parse::memory_linux(&raw))
                {
                    result.memory_used_bytes = Some(used);
                    result.memory_total_bytes = Some(total);
                }
            }
        }
        result
    }
}

#[cfg(any(target_os = "macos", test))]
async fn command(
    path: &Path,
    args: &[&str],
    limit: usize,
    stop: &CancellationToken,
    deadline: Instant,
) -> Option<Vec<u8>> {
    if stop.is_cancelled() || Instant::now() >= deadline {
        return None;
    }
    let spec = CommandSpec::new(path, limit, TIMEOUT)
        .args(args.iter().copied())
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    let runner = COMMANDS.get_or_init(|| CommandRunner::new(3).expect("finite metrics lanes"));
    let (cancel, receiver) = tokio::sync::oneshot::channel();
    let work = runner.run_cancelable(spec, receiver);
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = stop.cancelled() => { drop(cancel); let _=work.await; None },
        _ = tokio::time::sleep_until(deadline.into()) => { drop(cancel); let _=work.await; None },
        result = &mut work => result.ok().map(|r|r.stdout),
    }
}

#[cfg(any(target_os = "linux", test))]
fn read_cpu(path: &Path) -> Option<Vec<u8>> {
    // Linux places aggregate CPU counters first. Avoid reading all per-core
    // rows: large machines can exceed the unrelated meminfo byte budget.
    let mut value = Vec::new();
    let file = File::open(path).ok()?.take((TEXT_MAX + 1) as u64);
    BufReader::new(file).read_until(b'\n', &mut value).ok()?;
    (value.len() <= TEXT_MAX).then_some(value)
}

#[cfg(any(target_os = "linux", test))]
fn read(path: &Path) -> Option<Vec<u8>> {
    let mut value = Vec::new();
    File::open(path)
        .ok()?
        .take((TEXT_MAX + 1) as u64)
        .read_to_end(&mut value)
        .ok()?;
    (value.len() <= TEXT_MAX).then_some(value)
}

fn disk() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    let value = rustix::fs::statfs("/System/Volumes/Data")
        .or_else(|_| rustix::fs::statfs("/"))
        .ok()?;
    #[cfg(target_os = "linux")]
    let value = rustix::fs::statfs("/").ok()?;
    #[cfg(target_os = "macos")]
    let block_size = u64::from(value.f_bsize);
    #[cfg(target_os = "linux")]
    let block_size = u64::try_from(value.f_bsize).ok()?;
    parse::disk_bytes(value.f_blocks, value.f_bfree, block_size)
}

/// A watch holds just the last complete sample. No field falls back to an older
/// observation. The peer owns and joins this worker on disconnect/cancellation.
pub(crate) async fn run(
    collector: Arc<Collector>,
    latest: watch::Sender<Option<HostMetrics>>,
    stop: CancellationToken,
) {
    loop {
        if stop.is_cancelled() {
            return;
        }
        let value = collector.clone().sample(&stop).await;
        latest.send_replace(value);
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(INTERVAL) => {},
        }
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
