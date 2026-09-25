//! Bounded, shared process discovery. Only the caller-selected ps/lsof tools run;
//! every association is resolved from current ownership, never newest-file guesses.
use crate::{
    binding::{Binding, Provider, Status},
    process::{self, Snapshot},
    records,
};
use hmux_core::command::{CommandRunner, CommandSpec, RunErrorKind};
use hmux_model::Catalog;
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const OUTPUT_LIMIT: usize = 32 << 20;
const MAX_PANES: usize = 4096;
const FILE_LIMIT: usize = 16384;
const FILE_BYTES: usize = 4 << 20;
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static BACKGROUND: OnceLock<Arc<Semaphore>> = OnceLock::new();
static COMMANDS: OnceLock<CommandRunner> = OnceLock::new();
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Cancelled,
    Busy,
    Worker,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanPurpose {
    Metadata,
    Conversation,
    Completion,
}

#[derive(Clone)]
pub struct Inspector {
    home: PathBuf,
    ps: PathBuf,
    lsof: Option<PathBuf>,
}
impl std::fmt::Debug for Inspector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Inspector([redacted])")
    }
}
#[derive(Default)]
pub(crate) struct Metadata {
    runtime: String,
    model: String,
    state: String,
    process: String,
    working_since: i64,
}
pub(crate) struct Scan {
    pub bindings: BTreeMap<i32, Arc<Binding>>,
    pub statuses: BTreeMap<i32, Status>,
    metadata: BTreeMap<i32, Metadata>,
}
#[derive(Default)]
struct DiscoveryBudget {
    count: usize,
    bytes: usize,
}
pub(crate) fn admit() -> Option<OwnedSemaphorePermit> {
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .ok()
}
pub(crate) async fn wait_interactive(
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Option<OwnedSemaphorePermit> {
    let slots = SLOTS.get_or_init(|| Arc::new(Semaphore::new(2)));
    tokio::select! {
        biased;
        _ = stop.cancelled() => None,
        _ = tokio::time::sleep_until(deadline) => None,
        permit = slots.clone().acquire_owned() => permit.ok(),
    }
}
/// Catalog and completion share at most one of the two scan permits, leaving
/// one available for an interactive conversation even during background work.
pub(crate) struct BackgroundPermit {
    _background: OwnedSemaphorePermit,
    _slot: OwnedSemaphorePermit,
}
pub(crate) fn admit_background() -> Option<BackgroundPermit> {
    let slots = SLOTS.get_or_init(|| Arc::new(Semaphore::new(2)));
    let background = BACKGROUND.get_or_init(|| Arc::new(Semaphore::new(1)));
    admit_background_from(slots, background)
}
pub(crate) async fn wait_background(stop: &CancellationToken) -> Option<BackgroundPermit> {
    wait_background_inner(stop, None).await
}
pub(crate) async fn wait_background_for(
    stop: &CancellationToken,
    limit: Duration,
) -> Option<BackgroundPermit> {
    wait_background_inner(stop, Some(tokio::time::Instant::now() + limit)).await
}
async fn wait_background_inner(
    stop: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
) -> Option<BackgroundPermit> {
    let slots = SLOTS.get_or_init(|| Arc::new(Semaphore::new(2)));
    let background = BACKGROUND.get_or_init(|| Arc::new(Semaphore::new(1)));
    let background = tokio::select! {
        _ = stop.cancelled() => return None,
        _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } } => return None,
        permit = background.clone().acquire_owned() => permit.ok()?,
    };
    let slot = tokio::select! {
        _ = stop.cancelled() => return None,
        _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } } => return None,
        permit = slots.clone().acquire_owned() => permit.ok()?,
    };
    Some(BackgroundPermit {
        _background: background,
        _slot: slot,
    })
}
fn admit_background_from(
    slots: &Arc<Semaphore>,
    background: &Arc<Semaphore>,
) -> Option<BackgroundPermit> {
    let background = background.clone().try_acquire_owned().ok()?;
    let slot = slots.clone().try_acquire_owned().ok()?;
    Some(BackgroundPermit {
        _background: background,
        _slot: slot,
    })
}

pub(crate) fn commands() -> &'static CommandRunner {
    COMMANDS.get_or_init(|| CommandRunner::new(2).expect("nonzero inspection command limit"))
}
pub(crate) fn check(stop: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if stop.is_cancelled() || Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) async fn command(
    spec: CommandSpec,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    check(stop, deadline)?;
    let (cancel, receiver) = tokio::sync::oneshot::channel();
    let work = commands().run_cancelable(spec, receiver);
    tokio::pin!(work);
    tokio::select! {
        biased;
        _=stop.cancelled()=>{drop(cancel);let _=work.await;Err(Error::Cancelled)},
        _=tokio::time::sleep_until(deadline.into())=>{drop(cancel);let _=work.await;Err(Error::Cancelled)},
        result=&mut work=>result.map(|r|r.stdout).map_err(|error| if error.kind() == RunErrorKind::Busy { Error::Busy } else { Error::Unavailable }),
    }
}
impl Inspector {
    pub fn new(home: PathBuf, ps: PathBuf, lsof: Option<PathBuf>) -> Result<Self, Error> {
        if [&home, &ps].into_iter().chain(lsof.iter()).any(|p| {
            !p.is_absolute()
                || p.as_os_str().as_bytes().len() > 4096
                || p.as_os_str().as_bytes().contains(&0)
        }) {
            return Err(Error::Invalid);
        }
        Ok(Self { home, ps, lsof })
    }
    /// Synchronous work is called only while a process-wide blocking permit is held.
    pub(crate) fn scan(
        &self,
        panes: &[i32],
        purpose: ScanPurpose,
        stop: &CancellationToken,
        deadline: Instant,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Scan, Error> {
        check(stop, deadline)?;
        let scan_model = purpose == ScanPurpose::Metadata;
        if panes.len() > MAX_PANES || panes.iter().any(|&p| p < 1) {
            return Err(Error::Invalid);
        }
        let mut result = Scan {
            bindings: BTreeMap::new(),
            statuses: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };
        if panes.is_empty() {
            return Ok(result);
        }
        let graph = {
            let raw = runtime.block_on(command(
                CommandSpec::new(self.ps.clone(), OUTPUT_LIMIT, Duration::from_secs(5))
                    .args(["-axo", "pid=,ppid=,state=,pcpu=,comm="]),
                stop,
                deadline,
            ))?;
            Snapshot::parse(&raw).map_err(|_| Error::Unavailable)?
        };
        let mut owners = BTreeSet::new();
        let mut selected = BTreeMap::new();
        for &pane in panes {
            check(stop, deadline)?;
            let (pid, status) = graph.nearest_provider(pane);
            result.statuses.insert(pane, status);
            if pid > 0
                && status == Status::Ready
                && (purpose != ScanPurpose::Completion
                    || graph
                        .nodes
                        .get(&pid)
                        .is_some_and(|node| node.provider == Some(Provider::Codex)))
            {
                owners.insert(pid);
                selected.insert(pane, pid);
            }
        }
        let codex: BTreeSet<_> = owners
            .iter()
            .copied()
            .filter(|pid| {
                graph
                    .nodes
                    .get(pid)
                    .is_some_and(|node| node.provider == Some(Provider::Codex))
            })
            .collect();
        let mut budget = DiscoveryBudget::default();
        let initial = self.discover(&codex, &mut budget, stop, deadline, runtime);
        check(stop, deadline)?;
        if purpose == ScanPurpose::Conversation && matches!(initial, Err(Error::Busy)) {
            return Err(Error::Busy);
        }
        let known = initial.is_ok();
        let mut files = initial.unwrap_or_default();
        let mut wrappers = BTreeSet::new();
        if known {
            for &pid in &codex {
                if files.get(&pid).is_some_and(Vec::is_empty) {
                    let (chain, status) = graph.wrapper_chain(pid);
                    if status == Status::Ready {
                        wrappers.extend(chain);
                    }
                }
            }
        }
        let wrapper_files = self.discover(&wrappers, &mut budget, stop, deadline, runtime);
        check(stop, deadline)?;
        if purpose == ScanPurpose::Conversation && matches!(wrapper_files, Err(Error::Busy)) {
            return Err(Error::Busy);
        }
        let wrappers_known = wrapper_files.is_ok();
        if let Ok(values) = wrapper_files {
            files.extend(values);
        }
        let mut resolved = BTreeMap::new();
        for pid in owners {
            check(stop, deadline)?;
            let Some(provider) = graph.nodes.get(&pid).and_then(|node| node.provider) else {
                continue;
            };
            let mut binding = Binding::unavailable(provider, pid);
            match provider {
                Provider::Claude => {
                    binding = records::bind_claude(&self.home, binding, scan_model, stop, deadline)
                }
                Provider::Codex if known => {
                    binding = records::bind_codex(
                        binding,
                        pid,
                        files.get(&pid).map(Vec::as_slice).unwrap_or_default(),
                        scan_model,
                        stop,
                        deadline,
                    );
                    if binding.status == Status::Unavailable
                        && files.get(&pid).is_some_and(Vec::is_empty)
                        && wrappers_known
                    {
                        let (chain, status) = graph.wrapper_chain(pid);
                        if status == Status::Ready && chain.iter().all(|p| files.contains_key(p)) {
                            let mut found = None;
                            let mut ambiguous = false;
                            for child in chain {
                                check(stop, deadline)?;
                                let candidate = records::bind_codex(
                                    Binding::unavailable(provider, pid),
                                    child,
                                    files.get(&child).map(Vec::as_slice).unwrap_or_default(),
                                    scan_model,
                                    stop,
                                    deadline,
                                );
                                match candidate.status {
                                    Status::Ready => {
                                        if found.is_some() {
                                            ambiguous = true;
                                        } else {
                                            found = Some(candidate);
                                        }
                                    }
                                    Status::Ambiguous => ambiguous = true,
                                    Status::Unavailable => {}
                                }
                            }
                            if ambiguous {
                                binding.status = Status::Ambiguous;
                            } else if let Some(candidate) = found {
                                binding = candidate;
                            }
                        }
                    }
                }
                Provider::Codex => {}
            }
            resolved.insert(pid, Arc::new(binding));
        }
        for &pane in panes {
            if purpose == ScanPurpose::Completion {
                if let Some(binding) = selected.get(&pane).and_then(|pid| resolved.get(pid)) {
                    result.bindings.insert(pane, binding.clone());
                    result.statuses.insert(pane, binding.status);
                }
                continue;
            }
            if let Some(binding) = selected.get(&pane).and_then(|pid| resolved.get(pid)) {
                let node = &graph.nodes[&binding.provider_pid];
                let mut metadata = Metadata {
                    runtime: binding.provider.as_str().into(),
                    process: node.process.clone(),
                    state: process::infer_agent_state(node).into(),
                    ..Metadata::default()
                };
                if binding.status == Status::Ready {
                    metadata.model.clone_from(&binding.model);
                    if !binding.state.is_empty() {
                        metadata.state.clone_from(&binding.state);
                    }
                    metadata.working_since = binding.working_since;
                }
                result.metadata.insert(pane, metadata);
                result.statuses.insert(pane, binding.status);
                result.bindings.insert(pane, binding.clone());
            } else if let Some(node) = graph.candidate(pane) {
                result.metadata.insert(
                    pane,
                    Metadata {
                        runtime: "process".into(),
                        process: node.process.clone(),
                        state: "running".into(),
                        ..Metadata::default()
                    },
                );
            }
        }
        check(stop, deadline)?;
        Ok(result)
    }
    fn discover(
        &self,
        pids: &BTreeSet<i32>,
        budget: &mut DiscoveryBudget,
        stop: &CancellationToken,
        deadline: Instant,
        runtime: &tokio::runtime::Handle,
    ) -> Result<BTreeMap<i32, Vec<PathBuf>>, Error> {
        if pids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let path = self.lsof.as_ref().ok_or(Error::Unavailable)?;
        let mut files = BTreeMap::new();
        let pids: Vec<_> = pids.iter().copied().collect();
        // Batching wrapper descriptors avoids a process invocation for every tab.
        for batch in pids.chunks(256) {
            check(stop, deadline)?;
            let ids = batch
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let raw = runtime.block_on(command(
                CommandSpec::new(path.clone(), OUTPUT_LIMIT, Duration::from_secs(5))
                    .args(["-n", "-a", "-p", &ids, "-Fpn"])
                    .partial_exit(1),
                stop,
                deadline,
            ))?;
            let mut pid = None;
            for line in raw.split(|b| *b == b'\n') {
                check(stop, deadline)?;
                if let Some(value) = line.strip_prefix(b"p") {
                    pid = std::str::from_utf8(value)
                        .ok()
                        .and_then(|v| v.parse::<i32>().ok())
                        .filter(|p| batch.binary_search(p).is_ok());
                    // An observed process with no JSONL descriptors differs
                    // from a process absent from partial/inaccessible output.
                    if let Some(owner) = pid {
                        files.entry(owner).or_insert_with(Vec::new);
                    }
                    continue;
                }
                if let (Some(owner), Some(value)) = (pid, line.strip_prefix(b"n")) {
                    if !value.ends_with(b".jsonl") {
                        continue;
                    }
                    budget.count += 1;
                    budget.bytes += value.len();
                    if budget.count > FILE_LIMIT
                        || budget.bytes > FILE_BYTES
                        || value.len() > 8192
                        || value.contains(&0)
                    {
                        return Err(Error::Unavailable);
                    }
                    let value = std::str::from_utf8(value).map_err(|_| Error::Unavailable)?;
                    files
                        .entry(owner)
                        .or_insert_with(Vec::new)
                        .push(PathBuf::from(value));
                }
            }
        }
        Ok(files)
    }
    pub(crate) async fn annotate(
        self: Arc<Self>,
        mut catalog: Catalog,
        stop: &CancellationToken,
    ) -> Result<Catalog, Error> {
        let panes: BTreeSet<_> = catalog
            .sessions
            .iter()
            .flatten()
            .filter_map(|s| i32::try_from(s.pane_pid).ok().filter(|&p| p > 0))
            .collect();
        if panes.is_empty() {
            return Ok(catalog);
        }
        let Some(permit) = wait_background_for(stop, Duration::from_secs(2)).await else {
            return Ok(catalog);
        };
        let child = stop.child_token();
        let _cancel = child.clone().drop_guard();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let panes: Vec<_> = panes.into_iter().take(MAX_PANES).collect();
            if let Ok(scan) = self.scan(
                &panes,
                ScanPurpose::Metadata,
                &child,
                Instant::now() + Duration::from_secs(10),
                &runtime,
            ) {
                for session in catalog.sessions.iter_mut().flatten() {
                    if let Some(meta) = i32::try_from(session.pane_pid)
                        .ok()
                        .and_then(|pid| scan.metadata.get(&pid))
                    {
                        session.runtime.clone_from(&meta.runtime);
                        session.kind = if meta.runtime == "process" {
                            "shell".into()
                        } else {
                            meta.runtime.clone()
                        };
                        session.model.clone_from(&meta.model);
                        session.state.clone_from(&meta.state);
                        session.process.clone_from(&meta.process);
                        session.working_since = meta.working_since;
                    }
                }
            }
            Ok(catalog)
        })
        .await
        .map_err(|_| Error::Worker)?
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn background_scan_reserves_interactive_capacity_and_releases_both_permits() {
        let slots = Arc::new(Semaphore::new(2));
        let background = Arc::new(Semaphore::new(1));
        let catalog = admit_background_from(&slots, &background).unwrap();
        assert!(admit_background_from(&slots, &background).is_none());
        let conversation = slots.clone().try_acquire_owned().unwrap();
        assert!(slots.clone().try_acquire_owned().is_err());
        drop(catalog);
        let completion = admit_background_from(&slots, &background).unwrap();
        drop(conversation);
        drop(completion);
        assert_eq!(slots.available_permits(), 2);
        assert_eq!(background.available_permits(), 1);
    }
}
