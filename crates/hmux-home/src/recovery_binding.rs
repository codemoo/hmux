//! Two-pass provider authority for recovery. Never select a latest transcript.
use crate::{
    binding::{Provider, Status},
    catalog::TmuxCatalogReader,
    input_gate,
    inspection::{self, Inspector, ScanPurpose},
    recovery, status_probe,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub(crate) fn resolver(
    inspector: Arc<Inspector>,
    reader: TmuxCatalogReader,
    state_dir: PathBuf,
) -> recovery::Resolver {
    Arc::new(move |panes, purpose| {
        let inspector = inspector.clone();
        let reader = reader.clone();
        let state_dir = state_dir.clone();
        Box::pin(async move {
            let permit = inspection::admit_background().ok_or(recovery::Error::Busy)?;
            let stop = CancellationToken::new();
            let _guard = stop.clone().drop_guard();
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                // A restore may retain its exact journaled launch references
                // before daemon clients expose current descriptors. Ordinary
                // saves must reject every unresolved provider observation.
                let strict = purpose == recovery::ResolvePurpose::Checkpoint;
                let scan_purpose = if strict {
                    ScanPurpose::Recovery
                } else {
                    ScanPurpose::Conversation
                };
                let mut first = inspector
                    .scan(&panes, scan_purpose, &stop, deadline, &runtime)
                    .map_err(|_| recovery::Error::Unavailable)?;
                drop(permit);
                // Receipts exist only for this capture. Cached links, old status
                // panels and process continuity are never recovery authority.
                let receipts = if strict {
                    enrich(
                        &mut first, &inspector, &reader, &state_dir, &stop, deadline, &runtime,
                    )?
                } else {
                    BTreeMap::new()
                };
                let _permit = runtime
                    .block_on(inspection::wait_background_for(
                        &stop,
                        deadline.saturating_duration_since(Instant::now()),
                    ))
                    .ok_or(recovery::Error::Busy)?;
                let mut second = inspector
                    .scan(&panes, scan_purpose, &stop, deadline, &runtime)
                    .map_err(|_| recovery::Error::Unavailable)?;
                for (pane, receipt) in receipts {
                    let observed = second
                        .bindings
                        .get(&pane)
                        .ok_or(recovery::Error::Unavailable)?;
                    if observed.provider_pid != receipt.binding.provider_pid
                        || observed.provider != receipt.binding.provider
                        || (observed.status == Status::Ready
                            && !observed.same_record(&receipt.binding))
                        || observed.status == Status::Ambiguous
                        || !receipt.input.quiet()
                        || receipt.input.epoch.load(Ordering::SeqCst) != receipt.epoch
                        || inspector
                            .process_stamp(observed.provider_pid, &stop, deadline, &runtime)
                            .map_err(|_| recovery::Error::Unavailable)?
                            != receipt.stamp
                    {
                        return Err(recovery::Error::Unavailable);
                    }
                    second.bindings.insert(pane, receipt.binding);
                    second.statuses.insert(pane, Status::Ready);
                }
                inspection::check(&stop, deadline).map_err(|_| recovery::Error::Unavailable)?;
                if strict {
                    stable_checked(first, second)
                } else {
                    Ok(stable(first.bindings, &second.bindings))
                }
            })
            .await
            .map_err(|_| recovery::Error::Unavailable)?
        })
    })
}

struct Receipt {
    binding: Arc<crate::binding::Binding>,
    stamp: String,
    input: Arc<input_gate::State>,
    epoch: u64,
}
// Serial and bounded by the capture deadline; never spawn a worker per tab.
#[allow(clippy::too_many_arguments)]
fn enrich(
    scan: &mut inspection::Scan,
    inspector: &Inspector,
    reader: &TmuxCatalogReader,
    state_dir: &Path,
    stop: &CancellationToken,
    deadline: Instant,
    runtime: &tokio::runtime::Handle,
) -> Result<BTreeMap<i32, Receipt>, recovery::Error> {
    let mut receipts = BTreeMap::new();
    if !scan
        .bindings
        .values()
        .any(|b| b.provider == Provider::Codex && b.status == Status::Unavailable)
    {
        return Ok(receipts);
    }
    let catalog = runtime
        .block_on(reader.read_basic_cancelable(inspection::commands(), stop, deadline.into()))
        .map_err(|_| recovery::Error::Unavailable)?;
    let target = reader
        .terminal_target()
        .map_err(|_| recovery::Error::Unavailable)?;
    let raw = runtime
        .block_on(target.command_bounded(
            inspection::commands(),
            vec![
                "list-panes".into(),
                "-a".into(),
                "-F".into(),
                "#{session_id}|#{session_created}|#{window_id}|#{pane_id}|#{pane_pid}".into(),
            ],
            65536,
            stop,
            deadline.into(),
        ))
        .map_err(|_| recovery::Error::Unavailable)?;
    let rows = std::str::from_utf8(&raw).map_err(|_| recovery::Error::Unavailable)?;
    let mut seen = std::collections::BTreeSet::new();
    for row in rows.lines() {
        let f: Vec<_> = row.split('|').collect();
        if f.len() != 5 {
            return Err(recovery::Error::Unavailable);
        }
        let Some(session) = catalog
            .sessions
            .iter()
            .flatten()
            .find(|s| s.id == f[0] && s.created_at.to_string() == f[1])
        else {
            continue;
        };
        let Ok(pane) = f[4].parse::<i32>() else {
            return Err(recovery::Error::Unavailable);
        };
        let Some(base) = scan.bindings.get(&pane).cloned() else {
            continue;
        };
        if base.provider != Provider::Codex || base.status != Status::Unavailable {
            continue;
        }
        if !seen.insert(pane) {
            return Err(recovery::Error::Unavailable);
        }
        if !f[2]
            .strip_prefix('@')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            || !f[3]
                .strip_prefix('%')
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(recovery::Error::Unavailable);
        }
        let selector = format!("{}:{}.{}", f[0], f[2], f[3]);
        let identity = hmux_model::SessionIdentity {
            id: session.id.clone(),
            created_at: session.created_at,
        };
        let input = input_gate::state(&identity);
        let epoch = input.epoch.load(Ordering::SeqCst);
        let stamp = inspector
            .process_stamp(base.provider_pid, stop, deadline, runtime)
            .map_err(|_| recovery::Error::Unavailable)?;
        let probe = status_probe::Probe {
            selector: Some(&selector),
            reporter: None,
            identity: &identity,
            pane,
            base: &base,
            reader,
            inspector,
            state_dir,
            stop,
            deadline,
            runtime,
        };
        // A single unresolved provider already makes this whole save unsafe.
        // Stop here instead of spawning read commands for every remaining tab.
        let binding = Arc::new(
            probe
                .run_footer()
                .map_err(|_| recovery::Error::Unavailable)?,
        );
        scan.bindings.insert(pane, binding.clone());
        scan.statuses.insert(pane, Status::Ready);
        receipts.insert(
            pane,
            Receipt {
                binding,
                stamp,
                input,
                epoch,
            },
        );
    }
    Ok(receipts)
}
fn stable_checked(
    first: inspection::Scan,
    second: inspection::Scan,
) -> Result<BTreeMap<i32, recovery::ResumeReference>, recovery::Error> {
    if first.statuses != second.statuses
        || first.bindings.len() != second.bindings.len()
        || first
            .bindings
            .values()
            .chain(second.bindings.values())
            .any(|b| b.status != Status::Ready)
        || first
            .statuses
            .values()
            .chain(second.statuses.values())
            .any(|s| *s == Status::Ambiguous)
        || first
            .bindings
            .iter()
            .any(|(p, a)| second.bindings.get(p).is_none_or(|b| !a.same_record(b)))
    {
        return Err(recovery::Error::Unavailable);
    }
    Ok(stable(first.bindings, &second.bindings))
}

fn stable(
    first: BTreeMap<i32, Arc<crate::binding::Binding>>,
    second: &BTreeMap<i32, Arc<crate::binding::Binding>>,
) -> BTreeMap<i32, recovery::ResumeReference> {
    let mut refs = BTreeMap::new();
    for (pane, a) in first {
        let Some(b) = second.get(&pane) else { continue };
        if a.status != Status::Ready || b.status != Status::Ready || !a.same_record(b) {
            continue;
        }
        let Some(config_dir) = a.root.parent().and_then(|p| p.to_str()) else {
            continue;
        };
        refs.insert(
            pane,
            recovery::ResumeReference {
                provider: a.provider.as_str().into(),
                session_id: a.record_id.clone(),
                config_dir: config_dir.into(),
            },
        );
    }
    refs
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{Binding, Provider};
    #[test]
    fn only_stable_authoritative_records_can_be_resumed() {
        let a = Binding {
            provider: Provider::Claude,
            provider_pid: 20,
            file_pid: 20,
            path: "/synthetic/.claude/projects/a.jsonl".into(),
            root: "/synthetic/.claude/projects".into(),
            record_id: "synthetic-record".into(),
            model: String::new(),
            state: String::new(),
            working_since: 0,
            status: Status::Ready,
        };
        let first = BTreeMap::from([(10, Arc::new(a.clone()))]);
        let second = first.clone();
        let refs = stable(first.clone(), &second);
        assert_eq!(refs[&10].config_dir, "/synthetic/.claude");
        for replacement in [
            Binding {
                provider_pid: 21,
                ..a.clone()
            },
            Binding {
                record_id: "new-record".into(),
                ..a.clone()
            },
            Binding {
                status: Status::Ambiguous,
                ..a.clone()
            },
            Binding {
                path: "/synthetic/other".into(),
                ..a.clone()
            },
        ] {
            assert!(stable(
                first.clone(),
                &BTreeMap::from([(10, Arc::new(replacement))])
            )
            .is_empty());
        }
    }
}

#[cfg(test)]
#[path = "recovery_binding_tests.rs"]
mod scan_tests;
